import { defineStore } from "pinia";
import _ from "lodash";
import { addStoreHooks } from "@/utils/pinia_hooks_plugin";
import { useWorkspacesStore } from "@/store/workspaces.store";
<<<<<<< HEAD
import { useComponentsStore, ComponentId } from "@/store/components.store";
=======
import { ComponentId, useComponentsStore } from "@/store/components.store";
>>>>>>> e8bd300f (chore(web) app state cleanup)
import promiseDelay from "@/utils/promise_delay";
import { ApiRequest } from "@/utils/pinia_api_tools";
import hardcodedOutputs from "@/store/fixes/hardcoded_fix_outputs";
import { User } from "@/api/sdf/dal/user";
import { useAuthStore } from "@/store/auth.store";

export type FixStatus = "success" | "failure" | "running" | "unstarted";

export type FixId = number;
export type Fix = {
  id: FixId;
  name: string;
  componentName: string;
  componentId: ComponentId;
  recommendation: string;
  status: FixStatus;
  provider?: string;
  output?: string; // TODO(victor): output possibly comes from another endpoint, and should be linked at runtime. This is good for now.
  startedAt?: Date;
  finishedAt?: Date;
};

export type FixBatchId = number;
export type FixBatch = {
  id: FixBatchId;
  author: User;
  timestamp: Date;
};

export const useFixesStore = () => {
  const workspacesStore = useWorkspacesStore();
  const workspaceId = workspacesStore.selectedWorkspaceId;

  return addStoreHooks(
    defineStore(`w${workspaceId || "NONE"}/fixes`, {
      state: () => ({
        fixesById: {} as Record<FixId, Fix>,
        fixBatchIdsByFixId: {} as Record<FixId, FixBatchId>,
        fixBatchesById: {} as Record<FixBatchId, FixBatch>,
        processedFixComponents: 0,
        runningFixBatch: undefined as FixBatchId | undefined,
      }),
      getters: {
        allFixes(): Fix[] {
          return _.values(this.fixesById);
        },
        fixesByComponentId(): Record<ComponentId, Fix> {
          return _.keyBy(this.allFixes, (f) => f.componentId);
        },
        allFixBatches(): FixBatch[] {
          return _.values(this.fixBatchesById);
        },
        totalFixComponents() {
          const componentsStore = useComponentsStore();
          return componentsStore.allComponents.length;
        },
        fixesOnBatch() {
          return (fixBatchId: FixBatchId) => {
            const fixes = [];

            for (const fixId in this.fixBatchIdsByFixId) {
              if (this.fixBatchIdsByFixId[fixId] === fixBatchId) {
                fixes.push(this.fixesById[fixId]);
              }
            }

            return fixes;
          };
        },
        fixesOnRunningBatch(): Fix[] {
          if (!this.runningFixBatch) return [];

          return this.fixesOnBatch(this.runningFixBatch);
        },
        completedFixesOnRunningBatch(): Fix[] {
          return _.filter(
            this.fixesOnRunningBatch,
            (fix) => fix.status === "success",
          );
        },
        unstartedFixes(): Fix[] {
          return _.filter(this.allFixes, (fix) => fix.status === "unstarted");
        },
      },
      actions: {
        async LOAD_FIXES() {
          const componentsStore = useComponentsStore();

          if (
            !componentsStore.getRequestStatus("FETCH_COMPONENTS").value
              .isSuccess
          ) {
            await componentsStore.FETCH_COMPONENTS();
          }

          return new ApiRequest({
            url: "/session/get_defaults",
            onSuccess: (response) => {
              this.populateMockFixes().then(() => {});
            },
          });
        },
        async EXECUTE_FIXES(fixes: Array<Fix>) {
          return new ApiRequest({
            url: "/session/get_defaults",
            onSuccess: (response) => {
              this.executeMockFixes(fixes).then(() => {});
            },
          });
        },
        updateFix(fix: Fix) {
          this.fixesById[fix.id] = fix;
        },
        async populateMockFixes() {
          const componentsStore = useComponentsStore();

          for (const component of componentsStore.allComponents) {
            componentsStore.increaseActivityCounterOnComponent(component.id);
            await promiseDelay(1000);
            this.processedFixComponents += 1;

            if (
              [
                "Region",
                "Docker Image",
                "Butane",
                "Docker Hub Credential",
                "AMI",
              ].includes(component.schemaName)
            ) {
              componentsStore.decreaseActivityCounterOnComponent(component.id);
              continue;
            }

            // TODO(wendy+victor) - This system will eventually be replaced with something cleaner!
            const providers: Record<string, string> = {
              AMI: "AWS",
              "EC2 Instance": "AWS",
              Egress: "AWS",
              Ingress: "AWS",
              "Key Pair": "AWS",
              Region: "AWS",
              "Security Group": "AWS",
              Butane: "CoreOS",
              "Kubernetes Deployment": "Kubernetes",
              "Kubernetes Namespace": "Kubernetes",
            };
            const provider = providers[component.schemaName];

            this.updateFix({
              id: 1000 + component.id,
              componentId: component.id,
              name: `Create ${component.schemaName}`,
              componentName: component.displayName,
              componentId: component.id,
              recommendation:
                _.sample([
                  "this is what we recommend you do - just fix this thing and you will be all good",
                  "honestly idk, you figure it out",
                  "this one should be pretty simple",
                  "run this fix and you will be golden",
                  "don't just sit there, run the fix!",
                ]) ?? "",
              status: "unstarted",
              provider,
              output: hardcodedOutputs[component.schemaName] ?? "{}",
            });
            await promiseDelay(400); // Extra delay on items that will generate fixes
            componentsStore.decreaseActivityCounterOnComponent(component.id);
          }
        },
        async executeMockFixes(fixes: Array<Fix>) {
          const authStore = useAuthStore();
          const componentsStore = useComponentsStore();

          const fixBatch = <FixBatch>{
            id: _.random(100),
            author: authStore.user,
            timestamp: new Date(),
          };

          this.fixBatchesById[fixBatch.id] = fixBatch;

          this.runningFixBatch = fixBatch.id;

          for (const fix of fixes) {
            this.fixBatchIdsByFixId[fix.id] = fixBatch.id;
          }

          for (const fix of fixes) {
            await promiseDelay(200);

            this.updateFix({
              ...fix,
              startedAt: new Date(),
              status: "running",
            });

            componentsStore.increaseActivityCounterOnComponent(fix.componentId);

            await promiseDelay(2000);

            this.updateFix({
              ...fix,
              finishedAt: new Date(),
              status: "success",
            });
            componentsStore.decreaseActivityCounterOnComponent(fix.componentId);
          }

          this.runningFixBatch = undefined;
        },
      },
      async onActivated() {
        await this.LOAD_FIXES();
      },
    }),
  )();
};
