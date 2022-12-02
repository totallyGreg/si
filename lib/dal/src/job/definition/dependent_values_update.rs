use std::{collections::HashMap, convert::TryFrom};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use telemetry::prelude::*;
use tokio::task::JoinSet;

use crate::{
    job::consumer::{FaktoryJobInfo, JobConsumer, JobConsumerError, JobConsumerResult},
    job::producer::{JobMeta, JobProducer, JobProducerResult},
    ws_event::{AttributeValueStatusUpdate, StatusState, StatusValueKind},
    AccessBuilder, AttributeValue, AttributeValueError, AttributeValueId, AttributeValueResult,
    DalContext, ExternalProvider, FuncBackendKind, FuncBinding, FuncBindingError, Prop,
    SchemaVariant, StandardModel, Visibility, WsEvent,
};

#[derive(Debug, Deserialize, Serialize)]
struct DependentValuesUpdateArgs {
    attribute_value_id: AttributeValueId,
}

impl From<DependentValuesUpdate> for DependentValuesUpdateArgs {
    fn from(value: DependentValuesUpdate) -> Self {
        Self {
            attribute_value_id: value.attribute_value_id,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DependentValuesUpdate {
    attribute_value_id: AttributeValueId,
    access_builder: AccessBuilder,
    visibility: Visibility,
    faktory_job: Option<FaktoryJobInfo>,
}

impl DependentValuesUpdate {
    pub fn new(ctx: &DalContext, attribute_value_id: AttributeValueId) -> Box<Self> {
        let access_builder = AccessBuilder::from(ctx.clone());
        let visibility = *ctx.visibility();

        Box::new(Self {
            attribute_value_id,
            access_builder,
            visibility,
            faktory_job: None,
        })
    }
}

impl JobProducer for DependentValuesUpdate {
    fn args(&self) -> JobProducerResult<serde_json::Value> {
        Ok(serde_json::to_value(DependentValuesUpdateArgs::from(
            self.clone(),
        ))?)
    }

    fn meta(&self) -> JobProducerResult<JobMeta> {
        let mut custom = HashMap::new();
        custom.insert(
            "access_builder".to_string(),
            serde_json::to_value(self.access_builder.clone())?,
        );
        custom.insert(
            "visibility".to_string(),
            serde_json::to_value(self.visibility)?,
        );

        Ok(JobMeta {
            retry: Some(0),
            custom,
            ..JobMeta::default()
        })
    }

    fn identity(&self) -> String {
        serde_json::to_string(self).expect("Cannot serialize DependentValueUpdate")
    }
}

#[async_trait]
impl JobConsumer for DependentValuesUpdate {
    fn type_name(&self) -> String {
        "DependentValuesUpdate".to_string()
    }

    fn access_builder(&self) -> AccessBuilder {
        self.access_builder.clone()
    }

    fn visibility(&self) -> Visibility {
        self.visibility
    }

    async fn run(&self, ctx: &DalContext) -> JobConsumerResult<()> {
        let now = std::time::Instant::now();

        let mut source_attribute_value = AttributeValue::get_by_id(ctx, &self.attribute_value_id)
            .await?
            .ok_or_else(|| {
                AttributeValueError::NotFound(self.attribute_value_id, *ctx.visibility())
            })?;
        let mut dependency_graph = source_attribute_value.dependent_value_graph(ctx).await?;

        info!("{}", dependency_graph_to_dot(ctx, &dependency_graph).await?);

        // NOTE(nick,jacob): uncomment this for debugging.
        // Save printed output to a file and execute the following: "dot <file> -Tsvg -o <newfile>.svg"
        // println!("{}", dependency_graph_to_dot(ctx, &dependency_graph).await?);

        // Remove the `AttributeValueId` from the list of values that are in the dependencies,
        // as we consider that one to have already been updated. This lets us check for
        // `AttributeValuesId`s where the list of *unsatisfied* dependencies is empty.
        for (_, val) in dependency_graph.iter_mut() {
            val.retain(|&id| id != self.attribute_value_id);
        }
        info!(
            "DependentValuesUpdate for {:?}: dependency_graph {:?}",
            self.attribute_value_id, &dependency_graph
        );

        info!("{}", dependency_graph_to_dot(ctx, &dependency_graph).await?);

        let mut values_to_components: HashMap<AttributeValueId, AttributeValueStatusUpdate> =
            HashMap::new();

        for value_id in dependency_graph.keys() {
            let attribute_value = AttributeValue::get_by_id(ctx, value_id)
                .await?
                .ok_or_else(|| AttributeValueError::NotFound(*value_id, *ctx.visibility()))?;
            let component_id = attribute_value.context.component_id();

            let mut value_kind = StatusValueKind::Attribute;

            // TODO: only look up root prop once per component--take this out of the loop

            // does this value look like an output socket?
            if attribute_value
                .context
                .is_least_specific_field_kind_external_provider()
                .expect("TODO: attr context is invalid")
            {
                let external_provider = ExternalProvider::get_by_id(
                    ctx,
                    &attribute_value.context.external_provider_id(),
                )
                .await
                .expect("TODO: convert external provider err")
                .expect("TODO: external provider not found");
                let socket = external_provider
                    .sockets(ctx)
                    .await
                    .expect("TODO: failed to find sockets")
                    .pop()
                    .expect("TODO: no sockets in vec");
                value_kind = StatusValueKind::OutputSocket(*socket.id());
            }
            // does this value look like an input socket?
            if attribute_value
                .context
                .is_least_specific_field_kind_internal_provider()
                .expect("TODO: attr context is invalid")
            {}
            // does this value correspond to a code generation function?
            if attribute_value.context.prop_id().is_some() {
                let root_prop =
                    SchemaVariant::root_prop(ctx, attribute_value.context.schema_variant_id())
                        .await
                        .expect("TODO: convert error type");
                let prop = Prop::get_by_id(ctx, &dbg!(attribute_value.context).prop_id())
                    .await
                    .expect("TODO: error fetching prop")
                    .expect("TODO: prop not found ");
                if let Some(parent_prop) = prop
                    .parent_prop(ctx)
                    .await
                    .expect("TODO prop error convert")
                {
                    if let Some(grandparent_prop) = parent_prop
                        .parent_prop(ctx)
                        .await
                        .expect("TODO: prop error convert")
                    {
                        if grandparent_prop.id() == &root_prop.code_prop_id {
                            dbg!(grandparent_prop);
                            value_kind = StatusValueKind::CodeGen;
                        }
                    }
                }
            }

            values_to_components.insert(
                *value_id,
                AttributeValueStatusUpdate::new(*value_id, component_id, value_kind),
            );
        }

        let queued_values = values_to_components.values().copied().collect();

        WsEvent::status_update(ctx, StatusState::Queued, queued_values)
            .publish_immediately(ctx)
            .await?;

        let mut update_tasks = JoinSet::new();

        loop {
            // // If only HashMap.drain_filter were in stable...
            //
            // let satisfied_dependencies: HashMap<AttributeValueId, Vec<AttributeValueId>> =
            //     dependency_graph.drain_filter(|_, v| v.is_empty()).collect();
            //
            let mut satisfied_dependencies: Vec<AttributeValueId> =
                dependency_graph.keys().copied().collect();
            satisfied_dependencies.retain(|&id| {
                let result = if let Some(dependencies) = dependency_graph.get(&id) {
                    dependencies.is_empty()
                } else {
                    false
                };

                // We can go ahead and remove the entry in the dependency graph now,
                // since we know that all of its dependencies have been satisfied.
                // This also saves us from having to loop through the Vec again to
                // remove these entries immediately after this loop, anyway.
                if result {
                    dependency_graph.remove(&id);
                }

                result
            });

            let mut running_values = Vec::new();
            for value_id in satisfied_dependencies.iter() {
                running_values.push(
                    *values_to_components
                        .get(value_id)
                        // All attribute values have been iterated their component ids should be in
                        // the hashmap, otherwise this is a programmer bug!
                        .expect("component id not found for value id"),
                );
            }

            if !satisfied_dependencies.is_empty() {
                // Send a batched running status with all value/component ids that are being enqueued
                // for processing
                WsEvent::status_update(ctx, StatusState::Running, running_values)
                    .publish_immediately(ctx)
                    .await?;
            }

            for id in satisfied_dependencies {
                let attribute_value = AttributeValue::get_by_id(ctx, &id)
                    .await?
                    .ok_or_else(|| AttributeValueError::NotFound(id, *ctx.visibility()))?;
                let ctx_copy = ctx.clone();
                update_tasks
                    .build_task()
                    .name("AttributeValue.update_from_prototype_function")
                    .spawn(update_value(ctx_copy, attribute_value))?;
            }

            match update_tasks.join_next().await {
                Some(future_result) => {
                    // We get back a `Some<Result<Result<..>>>`. We've already unwrapped the
                    // `Some`, the outermost `Result` is a `JoinError` to let us know if
                    // anything went wrong in joining the task.
                    let finished_id = match future_result {
                        // We have successfully updated a value
                        Ok(Ok(finished_id)) => finished_id,
                        // There was an error (with our code) when updating the value
                        Ok(Err(err)) => {
                            warn!(error = ?err, "error updating value");
                            return Err(err.into());
                        }
                        // There was a Tokio JoinSet error when joining the task back (i.e. likely
                        // I/O error)
                        Err(err) => {
                            warn!(error = ?err, "error when joining update task");
                            return Err(err.into());
                        }
                    };

                    // Remove the `AttributeValueId` that just finished from the list of
                    // unsatisfied dependencies of all entries, so we can check what work
                    // has been unblocked.
                    for (_, val) in dependency_graph.iter_mut() {
                        val.retain(|&id| id != finished_id);
                    }

                    // Send a completed status for this value and *remove* it from the hash
                    WsEvent::status_update(
                        ctx,
                        StatusState::Completed,
                        vec![values_to_components
                            .remove(&finished_id)
                            // All attribute values have been iterated their component ids should be in
                            // the hashmap, otherwise this is a programmer bug!
                            .expect("component id not found for value id")],
                    )
                    .publish_immediately(ctx)
                    .await?;
                }
                // If we get `None` back from the `JoinSet` that means that there are no
                // further tasks in the `JoinSet` for us to wait on. This should only happen
                // after we've stopped adding new tasks to the `JoinSet`, which means either:
                //   * We have completely walked the initial graph, and have visited every
                //     node.
                //   * We've encountered a cycle that means we can no longer make any
                //     progress on walking the graph.
                // In both cases, there isn't anything more we can do, so we can stop looking
                // at the graph to find more work.
                None => break,
            }
        }

        // Send a completed status for all value/component ids that didn't need to be
        // queued/completed because they didn't have their dependencies met.
        dbg!(dependency_graph);
        WsEvent::status_update(
            ctx,
            StatusState::Completed,
            values_to_components.values().copied().collect(),
        )
        .publish_immediately(ctx)
        .await?;

        WsEvent::change_set_written(ctx).publish(ctx).await?;

        let elapsed_time = now.elapsed();
        info!(
            "DependentValuesUpdate for {:?} took {:?}",
            &self.attribute_value_id, elapsed_time
        );

        Ok(())
    }
}

/// Wrapper around `AttributeValue.update_from_prototype_function(&ctx)` to get it to
/// play more nicely with being spawned into a `JoinSet`.
async fn update_value(
    ctx: DalContext,
    mut attribute_value: AttributeValue,
) -> AttributeValueResult<AttributeValueId> {
    info!("DependentValueUpdate {:?}: START", attribute_value.id());
    let start = std::time::Instant::now();
    attribute_value.update_from_prototype_function(&ctx).await?;
    info!(
        "DependentValueUpdate {:?}: DONE {:?}",
        attribute_value.id(),
        start.elapsed()
    );

    Ok(*attribute_value.id())
}

impl TryFrom<faktory_async::Job> for DependentValuesUpdate {
    type Error = JobConsumerError;

    fn try_from(job: faktory_async::Job) -> Result<Self, Self::Error> {
        if job.args().len() != 3 {
            return Err(JobConsumerError::InvalidArguments(
                r#"[{ "attribute_value_id": <AttributeValueId> }, <AccessBuilder>, <Visibility>]"#
                    .to_string(),
                job.args().to_vec(),
            ));
        }
        let args: DependentValuesUpdateArgs = serde_json::from_value(job.args()[0].clone())?;
        let access_builder: AccessBuilder = serde_json::from_value(job.args()[1].clone())?;
        let visibility: Visibility = serde_json::from_value(job.args()[2].clone())?;

        let faktory_job_info = FaktoryJobInfo::try_from(job)?;

        Ok(Self {
            attribute_value_id: args.attribute_value_id,
            access_builder,
            visibility,
            faktory_job: Some(faktory_job_info),
        })
    }
}

#[allow(unused)]
async fn dependency_graph_to_dot(
    ctx: &DalContext,
    graph: &HashMap<AttributeValueId, Vec<AttributeValueId>>,
) -> AttributeValueResult<String> {
    let mut node_definitions = String::new();
    for attr_val_id in graph.keys() {
        let attr_val = AttributeValue::get_by_id(ctx, attr_val_id)
            .await?
            .ok_or_else(|| AttributeValueError::NotFound(*attr_val_id, *ctx.visibility()))?;
        node_definitions.push_str(&format!(
            "{node_id}[label=\"\\l{node_id:?}\\n\\n{context:#?}\"];",
            node_id = attr_val_id,
            context = attr_val.context,
        ));
    }

    let mut node_graph = String::new();
    for (attr_val, inputs) in graph {
        let dependencies = format!(
            "{{{dep_list}}}",
            dep_list = inputs
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<String>>()
                .join(" ")
        );
        let dependency_line = format!("{attr_val} -> {dependencies};",);
        node_graph.push_str(&dependency_line);
    }

    let dot_digraph = format!("digraph G {{{node_definitions}{node_graph}}}");

    Ok(dot_digraph)
}
