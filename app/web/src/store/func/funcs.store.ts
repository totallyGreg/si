import * as _ from "lodash-es";
import { defineStore } from "pinia";
import { addStoreHooks, ApiRequest } from "@si/vue-lib/pinia";

import storage from "local-storage-fallback"; // drop-in storage polyfill which falls back to cookies/memory
import { Visibility } from "@/api/sdf/dal/visibility";
import {
  FuncArgument,
  FuncArgumentKind,
  FuncKind,
  FuncId,
  FuncArgumentId,
  FuncSummary,
  FuncCode,
  FuncBinding,
  FuncBindingKind,
  Action,
  Attribute,
  CodeGeneration,
  Authentication,
  Qualification,
} from "@/api/sdf/dal/func";

import { nilId } from "@/utils/nilId";
import { trackEvent } from "@/utils/tracking";
import keyedDebouncer from "@/utils/keyedDebouncer";
import { useWorkspacesStore } from "@/store/workspaces.store";
import { useAssetStore } from "@/store/asset.store";
import { useChangeSetsStore } from "../change_sets.store";
import { useRealtimeStore } from "../realtime/realtime.store";
import { useComponentsStore } from "../components.store";

import {
  AttributePrototypeArgumentBag,
  CreateFuncOptions,
  FuncAssociations,
  InputSocketView,
  InputSourceProp,
  OutputLocation,
  OutputSocketView,
} from "./types";

import { FuncRunId } from "../func_runs.store";

type FuncExecutionState =
  | "Create"
  | "Dispatch"
  | "Failure"
  | "Run"
  | "Start"
  | "Success";

// TODO: remove when fn log stuff gets figured out a bit deeper
/* eslint-disable @typescript-eslint/no-explicit-any */
export type FuncExecutionLog = {
  id: FuncId;
  state: FuncExecutionState;
  value?: any;
  outputStream?: any[];
  functionFailure?: any; // FunctionResultFailure
};

export interface SaveFuncResponse {
  types: string;
  associations?: FuncAssociations;
}

export interface DeleteFuncResponse {
  success: boolean;
}

export interface OutputLocationOption {
  label: string;
  value: OutputLocation;
}

const LOCAL_STORAGE_FUNC_IDS_KEY = "si-open-func-ids";

export type InputSourceProps = { [key: string]: InputSourceProp[] };
export type InputSocketViews = { [key: string]: InputSocketView[] };
export type OutputSocketViews = { [key: string]: OutputSocketView[] };

export const useFuncStore = () => {
  const componentsStore = useComponentsStore();
  const changeSetsStore = useChangeSetsStore();
  const selectedChangeSetId = changeSetsStore.selectedChangeSet?.id;

  // TODO(nick): we need to allow for empty visibility here. Temporarily send down "nil" to mean that we want the
  // query to find the default change set.
  const visibility: Visibility = {
    visibility_change_set_pk:
      selectedChangeSetId ?? changeSetsStore.headChangeSetId ?? nilId(),
  };

  const workspacesStore = useWorkspacesStore();
  const workspaceId = workspacesStore.selectedWorkspacePk;

  let funcSaveDebouncer: ReturnType<typeof keyedDebouncer> | undefined;

  return addStoreHooks(
    defineStore(`ws${workspaceId || "NONE"}/cs${selectedChangeSetId}/funcs`, {
      state: () => ({
        // this powers the list
        funcsById: {} as Record<FuncId, FuncSummary>,
        funcArgumentsById: {} as Record<FuncArgumentId, FuncArgument>,
        funcArgumentsByFuncId: {} as Record<FuncId, FuncArgument[]>,
        // this is the code
        funcCodeById: {} as Record<FuncId, FuncCode>,
        // bindings
        actionBindings: {} as Record<FuncId, Action[]>,
        attributeBindings: {} as Record<FuncId, Attribute[]>,
        authenticationBindings: {} as Record<FuncId, Authentication[]>,
        codegenBindings: {} as Record<FuncId, CodeGeneration[]>,
        qualificationBindings: {} as Record<FuncId, Qualification[]>,
        // open editor tabs, this is duplicated in asset store
        openFuncIds: [] as FuncId[],
        // represents the last, or "focused" func clicked on/open by the editor
        selectedFuncId: undefined as FuncId | undefined,
      }),
      getters: {
        selectedFuncSummary(state): FuncSummary | undefined {
          return state.funcsById[this.selectedFuncId || ""];
        },
        selectedFuncCode(state): FuncCode | undefined {
          return state.funcCodeById[this.selectedFuncId || ""];
        },
        funcArguments(state): FuncArgument[] | undefined {
          return state.selectedFuncId
            ? state.funcArgumentsByFuncId[state.selectedFuncId]
            : undefined;
        },

        nameForSchemaVariantId: (_state) => (schemaVariantId: string) =>
          componentsStore.schemaVariantsById[schemaVariantId]?.schemaName,

        funcList: (state) => _.values(state.funcsById),
      },

      actions: {
        async FETCH_FUNC_LIST() {
          return new ApiRequest<{ funcs: FuncSummary[] }, Visibility>({
            url: "func/list_funcs",
            params: {
              ...visibility,
            },
            onSuccess: (response) => {
              this.funcsById = _.keyBy(response.funcs, (f) => f.funcId);
              this.recoverOpenFuncIds();
            },
          });
        },
        async FETCH_CODE(funcId: FuncId) {
          return new ApiRequest<FuncCode>({
            url: "func/get_code",
            params: {
              id: [funcId],
              ...visibility,
            },
            keyRequestStatusBy: funcId,
            onSuccess: (response) => {
              this.funcCodeById[response.funcId] = response;
            },
          });
        },
        async FETCH_BINDINGS(funcId: FuncId) {
          return new ApiRequest<FuncBinding[]>({
            url: "func/bindings",
            params: {
              id: funcId,
              ...visibility,
            },
            keyRequestStatusBy: funcId,
            onSuccess: (response) => {
              this.actionBindings[funcId] = [];
              this.attributeBindings[funcId] = [];
              this.authenticationBindings[funcId] = [];
              this.actionBindings[funcId] = [];

              response.forEach((binding) => {
                switch (binding.bindingKind) {
                  case FuncBindingKind.Action:
                    this.actionBindings[funcId]?.push(binding as Action);
                    break;
                  case FuncBindingKind.Attribute:
                    this.attributeBindings[funcId]?.push(binding as Attribute);
                    break;
                  case FuncBindingKind.Authentication:
                    this.authenticationBindings[funcId]?.push(
                      binding as Authentication,
                    );
                    break;
                  case FuncBindingKind.CodeGeneration:
                    this.codegenBindings[funcId]?.push(
                      binding as CodeGeneration,
                    );
                    break;
                  case FuncBindingKind.Qualification:
                    this.qualificationBindings[funcId]?.push(
                      binding as Qualification,
                    );
                    break;
                  default:
                    throw new Error(`Unexpected FuncBinding ${binding}`);
                }
              });
            },
          });
        },
        async DELETE_FUNC(funcId: FuncId) {
          return new ApiRequest<DeleteFuncResponse>({
            method: "post",
            url: "func/delete_func",
            params: {
              id: funcId,
              ...visibility,
            },
          });
        },
        async UPDATE_FUNC(func: FuncSummary) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;
          const isHead = changeSetsStore.headSelected;

          return new ApiRequest<SaveFuncResponse>({
            method: "post",
            url: "func/update_func",
            params: {
              ...func,
              ...visibility,
            },
            optimistic: () => {
              if (isHead) return () => {};

              const current = this.funcsById[func.funcId];
              return () => {
                if (current) {
                  this.funcsById[func.funcId] = current;
                } else {
                  delete this.funcCodeById[func.funcId];
                  delete this.funcsById[func.funcId];
                }
              };
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
            keyRequestStatusBy: func.funcId,
          });
        },
        async CREATE_ATTRIBUTE_PROTOTYPE(
          funcId: FuncId,
          schemaVariantId: string,
          prototypeArguments: AttributePrototypeArgumentBag[],
          componentId?: string,
          propId?: string,
          outputSocketId?: string,
        ) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<null>({
            method: "post",
            url: "func/create_attribute_prototype",
            params: {
              funcId,
              schemaVariantId,
              componentId,
              propId,
              outputSocketId,
              prototypeArguments,
              ...visibility,
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
        async UPDATE_ATTRIBUTE_PROTOTYPE(
          funcId: FuncId,
          attributePrototypeId: string,
          prototypeArguments: AttributePrototypeArgumentBag[],
          propId?: string,
          outputSocketId?: string,
        ) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<null>({
            method: "post",
            url: "func/update_attribute_prototype",
            params: {
              funcId,
              attributePrototypeId,
              propId,
              outputSocketId,
              prototypeArguments,
              ...visibility,
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
        async REMOVE_ATTRIBUTE_PROTOTYPE(attributePrototypeId: string) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<null>({
            method: "post",
            url: "func/remove_attribute_prototype",
            params: {
              attributePrototypeId,
              ...visibility,
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
        async CREATE_FUNC_ARGUMENT(
          funcId: FuncId,
          name: string,
          kind: FuncArgumentKind,
          elementKind?: FuncArgumentKind,
        ) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<null>({
            method: "post",
            url: "func/create_func_argument",
            params: {
              funcId,
              name,
              kind,
              elementKind,
              ...visibility,
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
        async UPDATE_FUNC_ARGUMENT(
          funcId: FuncId,
          funcArgumentId: FuncArgumentId,
          name: string,
          kind: FuncArgumentKind,
          elementKind?: FuncArgumentKind,
        ) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<null>({
            method: "post",
            url: "func/update_func_argument",
            params: {
              funcId,
              funcArgumentId,
              name,
              kind,
              elementKind,
              ...visibility,
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
        async DELETE_FUNC_ARGUMENT(
          funcId: FuncId,
          funcArgumentId: FuncArgumentId,
        ) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<null>({
            method: "post",
            url: "func/delete_func_argument",
            params: {
              funcId,
              funcArgumentId,
              ...visibility,
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
        async FETCH_FUNC_ARGUMENT_LIST(funcId: FuncId) {
          return new ApiRequest<{ funcArguments: FuncArgument[] }>({
            url: "func/list_func_arguments",
            params: {
              funcId,
              ...visibility,
            },
            onSuccess: (response) => {
              this.funcArgumentsByFuncId[funcId] = response.funcArguments;
              for (const argument of response.funcArguments) {
                this.funcArgumentsById[argument.id] = argument;
              }
            },
          });
        },
        async SAVE_AND_EXEC_FUNC(funcId: FuncId) {
          const func = this.funcsById[funcId];
          if (func) {
            trackEvent("func_save_and_exec", {
              id: func.funcId,
              name: func.name,
            });
          }

          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<SaveFuncResponse>({
            method: "post",
            url: "func/save_and_exec",
            keyRequestStatusBy: funcId,
            params: { ...func, ...visibility },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
        async TEST_EXECUTE(executeRequest: {
          id: FuncId;
          args: unknown;
          code: string;
          componentId: string;
        }) {
          const func = this.funcsById[executeRequest.id];
          if (func) {
            trackEvent("function_test_execute", {
              id: func.funcId,
              name: func.name,
            });
          }

          // why aren't we doing anything with the result of this?!
          return new ApiRequest<{
            funcRunId: FuncRunId;
          }>({
            method: "post",
            url: "func/test_execute",
            params: { ...executeRequest, ...visibility },
          });
        },
        async CREATE_FUNC(createFuncRequest: {
          kind: FuncKind;
          name?: string;
          options?: CreateFuncOptions;
        }) {
          if (changeSetsStore.creatingChangeSet)
            throw new Error("race, wait until the change set is created");
          if (changeSetsStore.headSelected)
            changeSetsStore.creatingChangeSet = true;

          return new ApiRequest<{ summary: FuncSummary; code: FuncCode }>({
            method: "post",
            url: "func/create_func",
            params: { ...createFuncRequest, ...visibility },
            onSuccess: (response) => {
              this.funcsById[response.summary.funcId] = response.summary;
              this.funcCodeById[response.code.funcId] = response.code;
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
        async FETCH_PROTOTYPE_ARGUMENTS(
          propId?: string,
          outputSocketId?: string,
        ) {
          return new ApiRequest<{
            preparedArguments: Record<string, unknown>;
          }>({
            url: "attribute/get_prototype_arguments",
            params: { propId, outputSocketId, ...visibility },
          });
        },

        async recoverOpenFuncIds() {
          // fetch the list of open funcs from localstorage
          const localStorageFuncIds = (
            storage.getItem(LOCAL_STORAGE_FUNC_IDS_KEY) ?? ""
          ).split(",") as FuncId[];
          // Filter out cached ids that don't correspond to funcs anymore
          const newOpenFuncIds = _.intersection(
            localStorageFuncIds,
            _.keys(this.funcsById),
          );
          if (!_.isEqual(newOpenFuncIds, this.openFuncIds)) {
            this.openFuncIds = newOpenFuncIds;
          }
        },

        setOpenFuncId(id: FuncId, isOpen: boolean, unshift?: boolean) {
          if (isOpen) {
            if (!this.openFuncIds.includes(id)) {
              this.openFuncIds[unshift ? "unshift" : "push"](id);
            }
          } else {
            const funcIndex = _.indexOf(this.openFuncIds, id);
            if (funcIndex >= 0) this.openFuncIds.splice(funcIndex, 1);
          }

          storage.setItem(
            LOCAL_STORAGE_FUNC_IDS_KEY,
            this.openFuncIds.join(","),
          );
        },

        updateFuncCode(funcId: FuncId, code: string) {
          const func = _.cloneDeep(this.funcCodeById[funcId]);
          if (!func || func.code === code) return;
          func.code = code;

          this.enqueueFuncSave(func);
        },

        enqueueFuncSave(func: FuncCode) {
          // do I even need this?
          // if (changeSetsStore.headSelected) return this.UPDATE_FUNC(func);

          this.funcCodeById[func.funcId] = func;

          // Lots of ways to handle this... we may want to handle this debouncing in the component itself
          // so the component has its own "draft" state that it passes back to the store when it's ready to save
          // however this should work for now, and lets the store handle this logic
          if (!funcSaveDebouncer) {
            funcSaveDebouncer = keyedDebouncer((id: FuncId) => {
              const f = this.funcCodeById[id];
              if (!f) return;
              this.SAVE_FUNC(f);
            }, 500);
          }
          // call debounced function which will trigger sending the save to the backend
          const saveFunc = funcSaveDebouncer(func.funcId);
          if (saveFunc) {
            saveFunc(func.funcId);
          }
        },

        SAVE_FUNC(func: FuncCode) {
          return new ApiRequest<FuncCode>({
            method: "post",
            url: "func/save_func",
            params: { ...func, ...visibility },
            onSuccess: (response) => {
              this.funcCodeById[response.funcId] = response;
            },
            onFail: () => {
              changeSetsStore.creatingChangeSet = false;
            },
          });
        },
      },
      onActivated() {
        this.FETCH_FUNC_LIST();

        const assetStore = useAssetStore();
        const realtimeStore = useRealtimeStore();

        realtimeStore.subscribe(this.$id, `changeset/${selectedChangeSetId}`, [
          {
            eventType: "ChangeSetWritten",
            callback: () => {
              this.FETCH_FUNC_LIST();
            },
          },
          // TODO(victor) we don't need the changeSetId checks below, since nats filters messages already
          {
            eventType: "FuncCreated",
            callback: (data) => {
              if (data.changeSetId !== selectedChangeSetId) return;
              this.FETCH_FUNC_LIST();
            },
          },
          {
            eventType: "FuncDeleted",
            callback: (data) => {
              if (data.changeSetId !== selectedChangeSetId) return;
              this.FETCH_FUNC_LIST();

              const assetId = assetStore.selectedVariantId;
              if (
                assetId &&
                this.selectedFuncId &&
                assetStore.selectedFuncs.includes(this.selectedFuncId)
              ) {
                assetStore.closeFunc(assetId, this.selectedFuncId);
              }
            },
          },
          {
            eventType: "FuncArgumentsSaved",
            callback: (data) => {
              if (data.changeSetId !== selectedChangeSetId) return;
              if (data.funcId !== this.selectedFuncId) return;
              this.FETCH_FUNC_ARGUMENT_LIST(data.funcId);
              // dont overwrite the code
              // this.FETCH_FUNC(data.funcId);
            },
          },
          {
            eventType: "FuncSaved",
            callback: (data) => {
              if (data.changeSetId !== selectedChangeSetId) return;
              // this.FETCH_FUNC_LIST();

              // Reload the last selected asset to ensure that its func list is up to date.
              // TODO: jobelenus, move this to the asset store!
              const assetId = assetStore.selectedVariantId;
              if (assetId) {
                assetStore.LOAD_SCHEMA_VARIANT(assetId);
              }

              if (this.selectedFuncId) {
                // Only fetch if we don't have the selected func in our state or if we are on HEAD.
                // If we are on HEAD, the func is immutable, so we are safe to fetch. However, if
                // we are not on HEAD, then the func is mutable. Therefore, we can only fetch
                // relevant metadata in order to avoid overwriting functions with their previous
                // value before the save queue is drained.
                if (data.funcId === this.selectedFuncId) {
                  if (
                    typeof this.funcCodeById[this.selectedFuncId] ===
                      "undefined" ||
                    changeSetsStore.headSelected
                  ) {
                    this.FETCH_CODE(this.selectedFuncId);
                  } else {
                    this.FETCH_BINDINGS(this.selectedFuncId);
                  }
                }
              }
            },
          },
        ]);
        return () => {
          realtimeStore.unsubscribe(this.$id);
        };
      },
    }),
  )();
};
