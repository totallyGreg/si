import { defineStore } from "pinia";
import _ from "lodash";
import async from "async";

import { addStoreHooks } from "@/utils/pinia_hooks_plugin";

import promiseDelay from "@/utils/promise_delay";
import { ChangeSetId, useChangeSetsStore } from "./change_sets.store";
import { useRealtimeStore } from "./realtime/realtime.store";

import { ComponentId } from "./components.store";

// NOTE - some uncertainty around transition from update finished state ("5/5 update complete") back to idle ("Model is up to date")
export type GlobalUpdateStatus = {
  // NOTE - might want an state enum here as well (for example to turn the bar into an error state)

  isUpdating: boolean;

  stepsCountCurrent: number;
  stepsCountTotal: number;

  componentsCountCurrent: number;
  componentsCountTotal: number;

  // not loving these names...
  updateStartedAt: Date; // timestamp when this update/batch was kicked off
  lastStepCompletedAt: Date; // timestamp of latest processed update within this cascade of updates
};

export type ComponentUpdateStatus = {
  componentId: number;

  isUpdating: boolean; // note - might change to enum if more states appear

  stepsCountCurrent: number;
  stepsCountTotal: number;

  statusMessage: string; // ex: updating attributes

  lastStepCompletedAt: Date;

  byActor?:
    | { type: "system" }
    | {
        type: "user";
        id: number;
        label: string;
      };
};

export type AttributeValueStatus = "queued" | "running" | "completed";

export type AttributeValueId = number;

export type UpdateStatusTimestamps = {
  queuedAt: Date;
  runningAt?: Date;
  completedAt?: Date;
};

export type ComponentStatusDetails = {
  lastUpdatedAt?: Date;
  timestampsByValueId: Record<AttributeValueId, UpdateStatusTimestamps>;
};

export const useStatusStore = (forceChangeSetId?: ChangeSetId) => {
  // this needs some work... but we'll probably want a way to force using HEAD
  // so we can load HEAD data in some scenarios while also loading a change set?
  let changeSetId: ChangeSetId | null;
  if (forceChangeSetId) {
    changeSetId = forceChangeSetId;
  } else {
    const changeSetsStore = useChangeSetsStore();
    changeSetId = changeSetsStore.selectedChangeSetId;
  }

  return addStoreHooks(
    defineStore(`cs${changeSetId || "NONE"}/status`, {
      state: () => ({
        valueStatusTimestampsByComponentId: {} as Record<
          ComponentId,
          ComponentStatusDetails
        >,
      }),
      getters: {
        latestComponentUpdate(): ComponentUpdateStatus | undefined {
          const sortedUpdates = _.orderBy(
            _.values(this.componentStatusById),
            (cu) => cu.lastStepCompletedAt,
          );
          return sortedUpdates.pop();
        },
        globalStatus(): GlobalUpdateStatus {
          const isUpdating = _.some(
            this.componentStatusById,
            (status) => status.isUpdating,
          );
          const stepsCountCurrent = _.sumBy(
            _.values(this.componentStatusById),
            (status) => status.stepsCountCurrent,
          );
          const stepsCountTotal = _.sumBy(
            _.values(this.componentStatusById),
            (status) => status.stepsCountTotal,
          );
          const componentsCountCurrent = _.filter(
            this.componentStatusById,
            (status) => !status.isUpdating,
          ).length;
          const componentsCountTotal = _.size(this.componentStatusById);

          return {
            isUpdating,
            stepsCountCurrent,
            stepsCountTotal,
            componentsCountCurrent,
            componentsCountTotal,
            // TODO(wendy) - fix these
            updateStartedAt: new Date(),
            lastStepCompletedAt: new Date(),
          };
        },
        valueStatusesByComponentId(state) {
          return _.mapValues(
            state.valueStatusTimestampsByComponentId,
            (valueStatusesById) =>
              _.mapValues(
                valueStatusesById.timestampsByValueId,
                (timestamps) => {
                  if (timestamps.completedAt) return "completed";
                  else if (timestamps.runningAt) return "running";
                  else return "queued";
                },
              ),
          );
        },
        componentStatusById(): Record<ComponentId, ComponentUpdateStatus> {
          return _.mapValues(
            this.valueStatusesByComponentId,
            (valueStatusesById, componentId) => {
              const stepsCountTotal = _.values(valueStatusesById).length;
              const stepsCountCurrent = _.filter(
                valueStatusesById,
                (status) => status === "completed",
              ).length;
              const isUpdating = stepsCountCurrent < stepsCountTotal;
              const lastStepCompletedAt =
                this.valueStatusTimestampsByComponentId[parseInt(componentId)]
                  ?.lastUpdatedAt || new Date(); // TODO(wendy) - fix

              return {
                componentId: parseInt(componentId),
                isUpdating,
                stepsCountCurrent,
                stepsCountTotal,
                statusMessage: isUpdating ? "updating" : "updated", // TODO(wendy) - more details here?
                lastStepCompletedAt,
              };
            },
          );
        },
      },
      actions: {
        async FETCH_CURRENT_STATUS() {
          // this.globalStatus = {
          //   isUpdating: false,
          // };
          // return new ApiRequest<{
          //   global: GlobalUpdateStatus;
          //   components: Record<ComponentId, ComponentUpdateStatus>;
          // }>({
          //   url: "/status",
          //   // TODO: do we want to pass these through as headers? or in URL?
          //   params: {
          //     workspaceId,
          //     changeSetId,
          //   },
          //   onSuccess: (response) => {
          //     this.globalStatus = response.global;
          //     this.componentStatusById = _.keyBy(response.components, "id");
          //   },
          // });
        },

        checkCompletedCleanup() {
          if (!this.globalStatus.isUpdating) {
            // if we're done updating, clear the timestamps
            _.each(this.valueStatusTimestampsByComponentId, (component) => {
              component.timestampsByValueId = {};
            });
          }
        },
      },
      onActivated() {
        if (!changeSetId) return;

        this.FETCH_CURRENT_STATUS();

        const realtimeStore = useRealtimeStore();
        let cleanupTimeout: Timeout;

        realtimeStore.subscribe(this.$id, `changeset/${changeSetId}`, [
          {
            eventType: "StatusUpdate",
            callback: (update) => {
              const now = new Date();
              update.values.forEach(({ componentId, valueId }) => {
                this.valueStatusTimestampsByComponentId[componentId] ||= {
                  timestampsByValueId: {},
                }; // if the given componentId doesn't have a corresponding object yet, make one
                const component =
                  this.valueStatusTimestampsByComponentId[componentId];

                // If we don't have a timestamp for an earlier step, we set it to the same one as the current step
                // If the status is queued, clear other statuses
                if (
                  update.status === "queued" ||
                  !component.timestampsByValueId[valueId]
                ) {
                  component.timestampsByValueId[valueId] = { queuedAt: now };
                }
                if (update.status === "completed") {
                  component.timestampsByValueId[valueId].completedAt = now;
                  component.timestampsByValueId[valueId].runningAt ||= now;
                  component.lastUpdatedAt = now;
                } else if (update.status === "running") {
                  component.timestampsByValueId[valueId].runningAt = now;
                  delete component.timestampsByValueId[valueId].completedAt;
                }
              });
              if (update.status === "completed") {
                if (cleanupTimeout) clearTimeout(cleanupTimeout);
                cleanupTimeout = setTimeout(this.checkCompletedCleanup, 2000);
              }
            },
          },

          // Old fake object
          // {
          //   eventType: "UpdateStatus",
          //   callback: (update) => {
          //     this.globalStatus = update.global;
          //     if (update.components) {
          //       _.each(update.components, (cu) => {
          //         this.componentStatusById[cu.componentId] = cu;
          //       });
          //     }
          //   },
          // },
        ]);

        return () => {
          clearTimeout(cleanupTimeout);
          realtimeStore.unsubscribe(this.$id);
        };
      },
    }),
  )();
};
