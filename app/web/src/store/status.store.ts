import { time } from "console";
import { defineStore } from "pinia";
import _ from "lodash";

import { addStoreHooks } from "@/utils/pinia_hooks_plugin";

import { ChangeSetId, useChangeSetsStore } from "./change_sets.store";
import { useRealtimeStore } from "./realtime/realtime.store";

import { ComponentId, SocketId } from "./components.store";

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

export type AttributeValueKind =
  | "internal"
  | "attribute"
  | "codeGen"
  | "qualification"
  | "confirmation"
  | "inputSocket"
  | "outputSocket";

export type ComponentStatusDetails = {
  lastUpdatedAt?: Date;
  valueKindByValueId: Record<
    AttributeValueId,
    { kind: AttributeValueKind; id?: number }
  >;
  message: string;
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
        statusDetailsByComponentId: {} as Record<
          ComponentId,
          ComponentStatusDetails
        >,
      }),
      getters: {
        getSocketStatus:
          (state) => (componentId: ComponentId, socketId: SocketId) => {
            const valueId = _.findKey(
              state.statusDetailsByComponentId[componentId]?.valueKindByValueId,
              (kindInfo) =>
                kindInfo.kind.endsWith("Socket") && kindInfo.id === socketId,
            );
            if (!valueId) return "idle";
            const timestamps =
              state.statusDetailsByComponentId[componentId]
                ?.timestampsByValueId[parseInt(valueId)];
            if (!timestamps) return "idle";
            if (timestamps.completedAt) return "completed";
            if (timestamps.runningAt) return "running";
            if (timestamps.queuedAt) return "queued";
          },

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
            state.statusDetailsByComponentId,
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
            (valueStatusesById, componentIdKey) => {
              const componentId = parseInt(componentIdKey);
              const stepsCountTotal = _.values(valueStatusesById).length;
              const stepsCountCurrent = _.filter(
                valueStatusesById,
                (status) => status === "completed",
              ).length;
              const isUpdating = stepsCountCurrent < stepsCountTotal;
              const lastStepCompletedAt =
                this.statusDetailsByComponentId[componentId]?.lastUpdatedAt ||
                new Date(); // TODO(wendy) - fix

              return {
                componentId,
                isUpdating,
                stepsCountCurrent,
                stepsCountTotal,
                // statusMessage: isUpdating ? "updating" : "updated", // TODO(wendy) - more details here?
                statusMessage: isUpdating
                  ? this.statusDetailsByComponentId[componentId]?.message
                  : "Component updated",
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
            _.each(this.statusDetailsByComponentId, (component) => {
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
              update.values.forEach(({ componentId, valueId, valueKind }) => {
                this.statusDetailsByComponentId[componentId] ||= {
                  message: "updating component",
                  timestampsByValueId: {},
                  valueKindByValueId: {},
                }; // if the given componentId doesn't have a corresponding object yet, make one
                const componentDetails =
                  this.statusDetailsByComponentId[componentId];

                componentDetails.valueKindByValueId[valueId] = valueKind;

                // If we don't have a timestamp for an earlier step, we set it to the same one as the current step
                // If the status is queued, clear other statuses
                if (
                  update.status === "queued" ||
                  !componentDetails.timestampsByValueId[valueId]
                ) {
                  componentDetails.timestampsByValueId[valueId] = {
                    queuedAt: now,
                  };
                }
                if (update.status === "completed") {
                  componentDetails.timestampsByValueId[valueId].completedAt =
                    now;
                  componentDetails.timestampsByValueId[valueId].runningAt ||=
                    now;
                  componentDetails.lastUpdatedAt = now;
                } else if (update.status === "running") {
                  componentDetails.timestampsByValueId[valueId].runningAt = now;
                  delete componentDetails.timestampsByValueId[valueId]
                    .completedAt;

                  if (valueKind.kind !== "internal") {
                    componentDetails.message = {
                      codeGen: "Running code gen",
                      attribute: "Updating attributes",
                      qualification: "Running qualifications",
                      inputSocket: "Updating input socket values",
                      outputSocket: "Updating output socket values",
                      confirmation: "Running confirmations",
                    }[valueKind.kind];
                  }
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
