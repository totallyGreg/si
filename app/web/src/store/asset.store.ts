import { watch } from "vue";
import { defineStore } from "pinia";
import * as _ from "lodash-es";
import { addStoreHooks, ApiRequest } from "@si/vue-lib/pinia";
import { useWorkspacesStore } from "@/store/workspaces.store";
import { FuncKind, FuncId } from "@/api/sdf/dal/func";
import { SchemaVariant, SchemaVariantId } from "@/api/sdf/dal/schema";
import { Visibility } from "@/api/sdf/dal/visibility";
import keyedDebouncer from "@/utils/keyedDebouncer";
import router from "@/router";
import { PropKind } from "@/api/sdf/dal/prop";
import { nonNullable } from "@/utils/typescriptLinter";
import { useFuncStore } from "./func/funcs.store";
import { useChangeSetsStore } from "./change_sets.store";
import { useRealtimeStore } from "./realtime/realtime.store";
import handleStoreError from "./errors";
import { useComponentsStore } from "./components.store";

export interface InstalledPkgAssetView {
  assetId: string;
  assetHash: string;
  assetKind: string;
}

export type DetachedAttributePrototypeKind =
  | {
      type: "OutputSocketSocket";
      data: {
        name: string;
        kind: "ConfigurationInput" | "ConfigurationOutput";
      };
    }
  | {
      type: "InputSocketSocket";
      data: {
        name: string;
        kind: "ConfigurationInput" | "ConfigurationOutput";
      };
    }
  | { type: "InputSocketProp"; data: { path: string; kind: PropKind } }
  | { type: "Prop"; data: { path: string; kind: PropKind } };

export interface DetachedAttributePrototype {
  id: string;
  funcId: FuncId;
  funcName: string;
  key: string | null;
  kind: FuncKind;
  context: DetachedAttributePrototypeKind;
}

export interface DetachedValidationPrototype {
  id: string;
  funcId: FuncId;
  funcName: string;
  args: unknown;
  link: string | null;
  propPath: string;
  propKind: PropKind;
}

export type SchemaVariantListEntry = SchemaVariant & {
  canUpdate: boolean;
  canContribute: boolean;
};
export type SchemaVariantSaveRequest = Visibility & { code?: string } & {
  variant: Omit<SchemaVariant, "created_at" | "updated_at">;
};
export type SchemaVariantCreateRequest = { name: string; color: string };
export type SchemaVariantCloneRequest = Visibility & {
  id: SchemaVariantId;
  name: string;
};

export const schemaVariantDisplayName = (schemaVariant: SchemaVariant) =>
  (schemaVariant.displayName ?? "").length === 0
    ? schemaVariant.schemaName
    : schemaVariant.displayName;

export const useAssetStore = () => {
  const changeSetsStore = useChangeSetsStore();
  const changeSetId = changeSetsStore.selectedChangeSetId;
  const visibility = {
    // changeSetId should not be empty if we are actually using this store
    // so we can give it a bad value and let it throw an error
    visibility_change_set_pk: changeSetId || "XXX",
  };

  const workspacesStore = useWorkspacesStore();
  const workspaceId = workspacesStore.selectedWorkspacePk;

  const funcsStore = useFuncStore();

  let assetSaveDebouncer: ReturnType<typeof keyedDebouncer> | undefined;

  return addStoreHooks(
    defineStore(`ws${workspaceId || "NONE"}/cs${changeSetId || "NONE"}/asset`, {
      state: () => ({
        variantList: [] as SchemaVariantListEntry[],
        variantsById: {} as Record<SchemaVariantId, SchemaVariant>,
        openVariantFuncIds: {} as { [key: SchemaVariantId]: FuncId[] },

        executeSchemaVariantTaskId: undefined as string | undefined,
        executeSchemaVariantTaskRunning: false as boolean,
        executeSchemaVariantTaskError: undefined as string | undefined,

        // represents state of the left rail lists and all open editor tabs
        selectedSchemaVariants: [] as SchemaVariantId[],
        selectedFuncs: [] as FuncId[],
        editingFuncLatestCode: {} as Record<SchemaVariantId, string>,

        detachmentWarnings: [] as {
          message: string;
          funcId: FuncId;
          kind?: FuncKind;
        }[],
      }),
      getters: {
        variantFromListById: (state) =>
          _.keyBy(state.variantList, (a) => a.schemaVariantId),
        schemaVariants: (state) => _.values(state.variantsById),
        selectedVariantId(state): SchemaVariantId | undefined {
          if (state.selectedSchemaVariants.length === 1)
            return state.selectedSchemaVariants[0];
          else return undefined;
        },
        selectedSchemaVariant(): SchemaVariant | undefined {
          if (this.selectedVariantId)
            return this.variantsById[this.selectedVariantId];
        },
        selectedSchemaVariantRecords(): SchemaVariantListEntry[] {
          return this.selectedSchemaVariants
            .map((id) => this.variantFromListById[id])
            .filter(nonNullable);
        },
        selectedFuncId(state): FuncId | undefined {
          if (state.selectedFuncs.length === 1) return state.selectedFuncs[0];
          else return undefined;
        },
      },
      actions: {
        addSchemaVariantSelection(id: SchemaVariantId) {
          if (!this.selectedSchemaVariants.includes(id)) {
            this.selectedSchemaVariants.push(id);
            // we don't load schema variant here, because we aren't showing the editor
            this.syncSelectionIntoUrl();
            this.selectedFuncs = [];
          }
        },
        setSchemaVariantSelection(id: SchemaVariantId) {
          if (
            this.selectedSchemaVariants.length === 1 &&
            this.selectedSchemaVariants[0] === id
          ) {
            return; // no-op
          }
          if (!this.selectedSchemaVariants.includes(id)) {
            this.selectedFuncs = [];
          }
          this.selectedSchemaVariants = [id];
          this.syncSelectionIntoUrl();
          if (this.variantFromListById[id]) {
            this.LOAD_SCHEMA_VARIANT(id);
          }
          // no last selected func
          funcsStore.selectedFuncId = undefined;
        },
        async addFuncSelection(id: FuncId) {
          if (!this.selectedFuncs.includes(id)) this.selectedFuncs.push(id);
          await funcsStore.FETCH_CODE(id);
          if (this.selectedSchemaVariant)
            this.openFunc(this.selectedSchemaVariant?.schemaVariantId, id);
          funcsStore.selectedFuncId = id;
          this.syncSelectionIntoUrl();
        },
        removeFuncSelection(id: FuncId) {
          const idx = this.selectedFuncs.indexOf(id);
          if (idx !== -1) this.selectedFuncs.splice(idx, 1);

          const idxx = funcsStore.openFuncIds.indexOf(id);
          if (idxx !== -1) funcsStore.openFuncIds.splice(idxx, 1);
          this.syncSelectionIntoUrl();
        },
        syncSelectionIntoUrl(returnQuery?: boolean) {
          let selectedIds: string[] = [];
          selectedIds = _.map(this.selectedSchemaVariants, (id) => `a_${id}`);
          selectedIds = selectedIds.concat(
            _.map(this.selectedFuncs, (id) => `f_${id}`),
          );

          const newQueryObj = {
            ...(selectedIds.length && { s: selectedIds.join("|") }),
          };
          if (returnQuery) return newQueryObj;

          if (!_.isEqual(router.currentRoute.value.query, newQueryObj)) {
            router.replace({
              query: newQueryObj,
            });
          }
        },
        async syncUrlIntoSelection() {
          this.selectedSchemaVariants = [];
          this.selectedFuncs = [];
          funcsStore.selectedFuncId = undefined;
          const ids = ((router.currentRoute.value.query?.s as string) || "")
            .split("|")
            .filter(Boolean);
          if (ids.length > 0) {
            /* eslint-disable @typescript-eslint/no-explicit-any */
            const promises: Promise<any>[] = [];
            const fnIds = [] as FuncId[];
            ids.sort().forEach((id) => {
              if (id.startsWith("a_")) {
                id = id.substring(2);
                this.selectedSchemaVariants.push(id);
                promises.push(this.LOAD_SCHEMA_VARIANT(id));
              } else if (id.startsWith("f_")) {
                id = id.substring(2);
                this.selectedFuncs.push(id);
                promises.push(funcsStore.FETCH_CODE(id));
                fnIds.push(id);
              }
            });
            await Promise.all(promises);
            for (const id of fnIds)
              if (this.selectedSchemaVariants[0])
                this.openFunc(this.selectedSchemaVariants[0], id);

            funcsStore.selectedFuncId = fnIds[fnIds.length - 1];
          }
        },

        openFunc(schemaVariantId: SchemaVariantId, funcId: FuncId) {
          const funcs = this.openVariantFuncIds[schemaVariantId] ?? [];
          if (!funcs.includes(funcId)) {
            funcs.push(funcId);
          }

          this.openVariantFuncIds[schemaVariantId] = funcs;
        },

        closeFunc(schemaVariantId: SchemaVariantId, funcId: FuncId) {
          const funcs = this.openVariantFuncIds[schemaVariantId] ?? [];
          this.openVariantFuncIds[schemaVariantId] = funcs.filter(
            (fId) => fId !== funcId,
          );
          this.removeFuncSelection(funcId);
        },

        // MOCK DATA GENERATION
        generateMockColor() {
          return `#${_.sample([
            "FF0000",
            "FFFF00",
            "FF00FF",
            "00FFFF",
            "FFAA00",
            "AAFF00",
            "00FFAA",
            "00AAFF",
          ])}`;
        },

        async CREATE_VARIANT(name: string) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetId === changeSetsStore.headChangeSetId)
            changeSetsStore.creatingChangeSet = true;
          return new ApiRequest<
            { id: SchemaVariantId; success: boolean },
            SchemaVariantCreateRequest
          >({
            method: "post",
            url: "/variant/create_variant",
            params: {
              ...visibility,
              name,
              color: this.generateMockColor(),
            },
          });
        },

        async CLONE_VARIANT(schemaVariantId: SchemaVariantId, name: string) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<
            { id: SchemaVariantId; success: boolean },
            SchemaVariantCloneRequest
          >({
            method: "post",
            keyRequestStatusBy: schemaVariantId,
            url: "/variant/clone_variant",
            params: {
              ...visibility,
              id: schemaVariantId,
              name,
            },
          });
        },

        enqueueVariantSave(schemaVariant: SchemaVariant, code: string) {
          this.editingFuncLatestCode[schemaVariant.schemaVariantId] = code;

          if (changeSetsStore.headSelected)
            return this.SAVE_SCHEMA_VARIANT(schemaVariant, code);

          this.variantsById[schemaVariant.schemaVariantId] = schemaVariant;

          if (!assetSaveDebouncer) {
            assetSaveDebouncer = keyedDebouncer((id: SchemaVariantId) => {
              const variant = this.variantsById[id];
              if (!variant) return;
              const code =
                this.editingFuncLatestCode[schemaVariant.schemaVariantId];

              if (!code)
                throw Error(
                  `No asset code for variant ${variant.schemaVariantId}`,
                );

              this.SAVE_SCHEMA_VARIANT(variant, code);
            }, 1000);
          }
          const assetSaveFunc = assetSaveDebouncer(
            schemaVariant.schemaVariantId,
          );
          if (assetSaveFunc) {
            assetSaveFunc(schemaVariant.schemaVariantId);
          }
        },

        async SAVE_SCHEMA_VARIANT(schemaVariant: SchemaVariant, code?: string) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;
          const isHead = changeSetsStore.headSelected;

          return new ApiRequest<
            { success: boolean; assetFuncId: FuncId },
            SchemaVariantSaveRequest
          >({
            method: "post",
            keyRequestStatusBy: schemaVariant.schemaVariantId,
            url: "/variant/save_variant",
            optimistic: () => {
              if (isHead) return () => {};

              const current = this.variantsById[schemaVariant.schemaVariantId];
              this.variantsById[schemaVariant.schemaVariantId] = schemaVariant;
              return () => {
                if (current) {
                  this.variantsById[schemaVariant.schemaVariantId] = current;
                } else {
                  delete this.variantsById[schemaVariant.schemaVariantId];
                }
              };
            },
            params: {
              ...visibility,
              code,
              variant: schemaVariant,
            },
          });
        },
        async REGENERATE_VARIANT(schemaVariantId: SchemaVariantId) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          this.detachmentWarnings = [];
          const variant = this.variantsById[schemaVariantId];
          if (!variant)
            throw new Error(`${schemaVariantId} Variant does not exist`);

          const code = this.editingFuncLatestCode[schemaVariantId];

          if (!code) throw new Error(`${schemaVariantId} Code does not exist`);

          return new ApiRequest<null>({
            method: "post",
            url: "/variant/regenerate_variant",
            keyRequestStatusBy: schemaVariantId,
            params: {
              ...visibility,
              variant,
              code,
            },
          });
        },

        async LOAD_SCHEMA_VARIANT(schemaVariantId: SchemaVariantId) {
          // when we load a variant, load all its code ahead of time before a user selects a func
          const variant = this.variantFromListById[schemaVariantId];
          if (variant) {
            await funcsStore.FETCH_CODE(variant.assetFuncId);

            const code = funcsStore.funcCodeById[variant.assetFuncId]?.code;

            if (code) {
              this.editingFuncLatestCode[schemaVariantId] = code;
            }
          }

          // its likely we no longer need this call, because this data is identical to the list data we already have
          return new ApiRequest<
            SchemaVariant,
            Visibility & {
              id: SchemaVariantId;
            }
          >({
            url: `v2/workspaces/${workspaceId}/change-sets/${changeSetId}/schema-variants/${schemaVariantId}`,
            keyRequestStatusBy: schemaVariantId,
            params: {
              id: schemaVariantId,
              ...visibility,
            },
            onSuccess: (response) => {
              this.variantsById[response.schemaVariantId] = response;
            },
          });
        },

        async LOAD_SCHEMA_VARIANT_LIST() {
          return new ApiRequest<SchemaVariant[], Visibility>({
            url: `v2/workspaces/${workspaceId}/change-sets/${changeSetId}/schema-variants`,
            params: { ...visibility },
            onSuccess: (response) => {
              this.variantList = response.map((v) => {
                const e = v as SchemaVariantListEntry;
                e.canContribute = false;
                e.canUpdate = false;
                return e;
              });
            },
          });
        },

        async CREATE_UNLOCKED_COPY(id: SchemaVariantId) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          this.detachmentWarnings = [];

          return new ApiRequest<{ id: string }>({
            method: "post",
            url: "/variant/create_unlocked_copy",
            keyRequestStatusBy: id,
            params: {
              ...visibility,
              id,
            },
          });
        },
      },
      async onActivated() {
        await this.LOAD_SCHEMA_VARIANT_LIST();
        const stopWatchingUrl = watch(
          () => {
            return router.currentRoute.value.name;
          },
          () => {
            if (
              router.currentRoute.value.name === "workspace-lab-assets" &&
              Object.values(router.currentRoute.value.query).length > 0
            ) {
              this.syncUrlIntoSelection(); // handles PAGE LOAD
            }
          },
          {
            immediate: true,
          },
        );

        const realtimeStore = useRealtimeStore();
        realtimeStore.subscribe(this.$id, `changeset/${changeSetId}`, [
          {
            eventType: "SchemaVariantCreated",
            callback: (variant, metadata) => {
              if (metadata.change_set_id !== changeSetId) return;
              const v = variant as SchemaVariantListEntry;
              v.canContribute = false;
              v.canUpdate = false;
              this.variantList.push(v);
            },
          },
          {
            eventType: "SchemaVariantCloned",
            callback: (data) => {
              if (data.changeSetId !== changeSetId) return;
              this.LOAD_SCHEMA_VARIANT_LIST();
            },
          },
          {
            eventType: "SchemaVariantSaved",
            callback: (data) => {
              if (data.changeSetId !== changeSetId) return;
              const savedAssetIdx = this.variantList.findIndex(
                (a) => a.schemaVariantId === data.schemaVariantId,
              );
              const savedAsset = this.variantList[savedAssetIdx];
              if (savedAsset) {
                savedAsset.schemaName = data.name;
                savedAsset.category = data.category;
                savedAsset.color = data.color;
                savedAsset.componentType = data.componentType;
                savedAsset.displayName = data.displayName || null;
                this.variantList.splice(savedAssetIdx, 1, savedAsset);
              }

              const existingAsset =
                this.variantFromListById[data.schemaVariantId];
              if (existingAsset) {
                existingAsset.schemaName = data.name;
                existingAsset.category = data.category;
                existingAsset.color = data.color;
                existingAsset.componentType = data.componentType;
                existingAsset.displayName = data.displayName || null;
                existingAsset.description = data.description || "";
                existingAsset.link = data.link || null;
              }
            },
          },
          {
            eventType: "SchemaVariantUpdateFinished",
            callback: async (data) => {
              if (data.changeSetId !== changeSetId) return;
              for (const variant of Object.values(this.variantsById)) {
                if (variant.schemaVariantId === data.oldSchemaVariantId) {
                  variant.schemaVariantId = data.newSchemaVariantId;
                }
              }
              this.LOAD_SCHEMA_VARIANT_LIST();

              if (this.selectedVariantId) {
                this.LOAD_SCHEMA_VARIANT(this.selectedVariantId);
                await useComponentsStore().FETCH_AVAILABLE_SCHEMAS();
              }
            },
          },
          {
            eventType: "ChangeSetApplied",
            callback: () => {
              this.LOAD_SCHEMA_VARIANT_LIST();
            },
          },
          // For the async api endpoints
          {
            eventType: "AsyncError",
            callback: ({ id, error }) => {
              if (id === this.executeSchemaVariantTaskId) {
                this.executeSchemaVariantTaskRunning = false;
                this.executeSchemaVariantTaskId = undefined;

                let errorMessage = error;
                {
                  const match = error.match(
                    "function execution result failure:.*message=(.*?),",
                  )?.[1];

                  if (match) {
                    errorMessage = match;
                  }
                }
                {
                  const match = error.match(
                    "func execution failure error: (.*)",
                  )?.[1];

                  if (match) {
                    errorMessage = match;
                  }
                }
                this.executeSchemaVariantTaskError = errorMessage;
              }
            },
          },
        ]);

        const actionUnsub = this.$onAction(handleStoreError);
        return () => {
          stopWatchingUrl();
          actionUnsub();
          realtimeStore.unsubscribe(this.$id);
        };
      },
    }),
  )();
};
