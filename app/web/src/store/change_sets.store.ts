import { defineStore } from "pinia";
import * as _ from "lodash-es";
import { watch } from "vue";
import { ApiRequest, addStoreHooks } from "@si/vue-lib/pinia";
import { useToast } from "vue-toastification";
import {
  ChangeSet,
  ChangeSetId,
  ChangeSetStatus,
} from "@/api/sdf/dal/change_set";
import router from "@/router";
import { UserId } from "@/store/auth.store";
import IncomingChangesMerging from "@/components/toasts/IncomingChangesMerging.vue";
import MovedToHead from "@/components/toasts/MovedToHead.vue";
import RebaseOnBase from "@/components/toasts/RebaseOnBase.vue";
import { useWorkspacesStore } from "./workspaces.store";
import { useRealtimeStore } from "./realtime/realtime.store";
import { useRouterStore } from "./router.store";
import handleStoreError from "./errors";
import { useStatusStore } from "./status.store";

const toast = useToast();

export interface StatusWithBase {
  baseHasUpdates: boolean;
  changeSetHasUpdates: boolean;
  conflictsWithBase: boolean;
}

export interface OpenChangeSetsView {
  headChangeSetId: ChangeSetId;
  changeSets: ChangeSet[];
}

export function useChangeSetsStore() {
  const workspacesStore = useWorkspacesStore();
  const workspacePk = workspacesStore.selectedWorkspacePk;

  return addStoreHooks(
    workspacePk,
    undefined,
    defineStore(`w${workspacePk || "NONE"}/change-sets`, {
      state: () => ({
        headChangeSetId: null as ChangeSetId | null,
        changeSetsById: {} as Record<ChangeSetId, ChangeSet>,
        changeSetsWrittenAtById: {} as Record<ChangeSetId, Date>,
        creatingChangeSet: false as boolean,
        postApplyActor: null as string | null,
        postAbandonActor: null as string | null,
        changeSetApprovals: {} as Record<UserId, string>,
        statusWithBase: {} as Record<ChangeSetId, StatusWithBase>,
      }),
      getters: {
        allChangeSets: (state) => _.values(state.changeSetsById),
        openChangeSets(): ChangeSet[] {
          return _.filter(this.allChangeSets, (cs) =>
            [
              ChangeSetStatus.Open,
              ChangeSetStatus.NeedsApproval,
              ChangeSetStatus.NeedsAbandonApproval,
            ].includes(cs.status),
          );
        },
        urlSelectedChangeSetId(): ChangeSetId | undefined {
          const route = useRouterStore().currentRoute;
          const id = route?.params?.changeSetId as ChangeSetId | undefined;
          if (id === "head" && this.headChangeSetId) {
            return this.headChangeSetId;
          }
          return id;
        },
        selectedChangeSet(): ChangeSet | null {
          return this.changeSetsById[this.urlSelectedChangeSetId || ""] || null;
        },
        headSelected(): boolean {
          if (this.headChangeSetId) {
            return this.urlSelectedChangeSetId === this.headChangeSetId;
          }
          return false;
        },

        selectedChangeSetLastWrittenAt(): Date | null {
          return (
            this.changeSetsWrittenAtById[this.selectedChangeSet?.id || ""] ??
            null
          );
        },

        selectedChangeSetId(): ChangeSetId | undefined {
          return this.selectedChangeSet?.id;
        },

        // expose here so other stores can get it without needing to call useWorkspaceStore directly
        selectedWorkspacePk: () => workspacePk,
      },
      actions: {
        async setActiveChangeset(changeSetId: string) {
          // We need to force refetch changesets since there's a race condition in which redirects
          // will be triggered but the frontend won't have refreshed the list of changesets
          if (!this.changeSetsById[changeSetId]) {
            await this.FETCH_CHANGE_SETS();
          }

          const route = router.currentRoute.value;
          await router.push({
            name: route.name ?? undefined,
            params: {
              ...route.params,
              changeSetId,
            },
          });

          const statusStore = useStatusStore(changeSetId);
          statusStore.resetWhenChangingChangeset();
        },

        async FETCH_CHANGE_SETS() {
          return new ApiRequest<OpenChangeSetsView>({
            url: "change_set/list_open_change_sets",
            onSuccess: (response) => {
              this.headChangeSetId = response.headChangeSetId;
              this.changeSetsById = _.keyBy(response.changeSets, "id");
            },
          });
        },
        async CREATE_CHANGE_SET(name: string) {
          return new ApiRequest<{ changeSet: ChangeSet }>({
            method: "post",
            url: "change_set/create_change_set",
            params: {
              changeSetName: name,
            },
            onSuccess: (response) => {
              this.changeSetsById[response.changeSet.id] = response.changeSet;
            },
          });
        },
        async ABANDON_CHANGE_SET() {
          if (this.creatingChangeSet)
            throw new Error("Wait until change set is created to abandon");
          if (!this.selectedChangeSet) throw new Error("Select a change set");
          else if (this.headSelected) {
            throw new Error("You cannot abandon HEAD!");
          }
          if (
            router.currentRoute.value.name &&
            ["workspace-lab-packages", "workspace-lab-assets"].includes(
              router.currentRoute.value.name.toString(),
            )
          ) {
            router.push({ name: "workspace-lab" });
          }
          return new ApiRequest<{ changeSet: ChangeSet }>({
            method: "post",
            url: "change_set/abandon_change_set",
            params: {
              changeSetId: this.selectedChangeSet.id,
            },
            optimistic: () => {
              // remove component selections, its corrupting navigation
              const key = `${this.selectedChangeSetId}_selected_component`;
              window.localStorage.removeItem(key);
              const headkey = `${this.headChangeSetId}_selected_component`;
              window.localStorage.removeItem(headkey);
            },
            onSuccess: (response) => {
              // this.changeSetsById[response.changeSet.pk] = response.changeSet;
              const statusStore = useStatusStore();
              statusStore.resetWhenChangingChangeset();
            },
          });
        },
        async FETCH_STATUS_WITH_BASE(changeSetId: ChangeSetId) {
          // do not call this with `head`
          if (changeSetId === "head") return Promise.resolve();
          return new ApiRequest<StatusWithBase>({
            method: "post",
            url: "change_set/status_with_base",
            params: {
              visibility_change_set_pk: changeSetId,
            },
            onSuccess: (data) => {
              this.statusWithBase[changeSetId] = data;
            },
          });
        },
        async REBASE_ON_BASE(changeSetId: ChangeSetId) {
          return new ApiRequest({
            method: "post",
            url: "change_set/rebase_on_base",
            params: {
              visibility_change_set_pk: changeSetId,
            },
            optimistic: () => {
              toast({
                component: IncomingChangesMerging,
                props: {
                  username: "HEAD",
                },
              });
            },
          });
        },
        async APPLY_CHANGE_SET(username: string) {
          if (!this.selectedChangeSet) throw new Error("Select a change set");
          return new ApiRequest<{ changeSet: ChangeSet }>({
            method: "post",
            url: "change_set/apply_change_set",
            params: {
              visibility_change_set_pk: this.selectedChangeSet.id,
            },
            optimistic: () => {
              toast({
                component: IncomingChangesMerging,
                props: {
                  username,
                },
              });
            },
            _delay: 2000,
            onSuccess: (response) => {
              this.changeSetsById[response.changeSet.id] = response.changeSet;
            },
            onFail: () => {
              // todo: show something!
            },
          });
        },
        async APPLY_CHANGE_SET_VOTE(vote: string) {
          if (!this.selectedChangeSet) throw new Error("Select a change set");
          return new ApiRequest({
            method: "post",
            url: "change_set/merge_vote",
            params: {
              vote,
              visibility_change_set_pk: this.selectedChangeSet.id,
            },
          });
        },
        async BEGIN_APPROVAL_PROCESS() {
          if (!this.selectedChangeSet) throw new Error("Select a change set");
          return new ApiRequest({
            method: "post",
            url: "change_set/begin_approval_process",
            params: {
              visibility_change_set_pk: this.selectedChangeSet.id,
            },
          });
        },
        async CANCEL_APPROVAL_PROCESS() {
          if (!this.selectedChangeSet) throw new Error("Select a change set");
          return new ApiRequest({
            method: "post",
            url: "change_set/cancel_approval_process",
            params: {
              visibility_change_set_pk: this.selectedChangeSet.id,
            },
          });
        },
        async APPLY_ABANDON_VOTE(vote: string) {
          if (!this.selectedChangeSet) throw new Error("Select a change set");
          return new ApiRequest({
            method: "post",
            url: "change_set/abandon_vote",
            params: {
              vote,
              visibility_change_set_pk: this.selectedChangeSet.id,
            },
          });
        },
        async BEGIN_ABANDON_APPROVAL_PROCESS() {
          if (!this.selectedChangeSet) throw new Error("Select a change set");
          return new ApiRequest({
            method: "post",
            url: "change_set/begin_abandon_approval_process",
            params: {
              visibility_change_set_pk: this.selectedChangeSet.id,
            },
          });
        },
        async CANCEL_ABANDON_APPROVAL_PROCESS() {
          if (!this.selectedChangeSet) throw new Error("Select a change set");
          return new ApiRequest({
            method: "post",
            url: "change_set/cancel_abandon_approval_process",
            params: {
              visibility_change_set_pk: this.selectedChangeSet.id,
            },
          });
        },

        getAutoSelectedChangeSetId() {
          const lastChangeSetId = sessionStorage.getItem(
            `SI:LAST_CHANGE_SET/${workspacePk}`,
          );
          if (
            lastChangeSetId &&
            this.changeSetsById[lastChangeSetId]?.status ===
              ChangeSetStatus.Open
          ) {
            return lastChangeSetId;
          }

          if (this.openChangeSets?.length <= 2) {
            // will select the single open change set or head if thats all that exists
            // eslint-disable-next-line @typescript-eslint/no-non-null-assertion
            return _.last(this.openChangeSets)!.id;
          }

          return this.headChangeSetId ?? false;
        },
        getGeneratedChangesetName() {
          let latestNum = 0;
          _.each(this.allChangeSets, (cs) => {
            const labelNum = Number(cs.name.split(" ").pop());
            if (!_.isNaN(labelNum) && labelNum > latestNum) {
              latestNum = labelNum;
            }
          });
          return `Change Set ${latestNum + 1}`;
        },
      },
      onActivated() {
        if (!workspacePk) return;
        this.FETCH_CHANGE_SETS();
        const stopWatchSelectedChangeSet = watch(
          () => this.selectedChangeSet,
          () => {
            // store last used change set (per workspace) in localstorage
            if (this.selectedChangeSet && workspacePk) {
              sessionStorage.setItem(
                `SI:LAST_CHANGE_SET/${workspacePk}`,
                this.selectedChangeSet.id,
              );
            }
          },
          { immediate: true },
        );

        const realtimeStore = useRealtimeStore();
        realtimeStore.subscribe(this.$id, `workspace/${workspacePk}`, [
          {
            eventType: "ChangeSetCreated",
            callback: this.FETCH_CHANGE_SETS,
          },
          {
            eventType: "ChangeSetAbandoned",
            callback: async (data) => {
              const changeSetName = this.selectedChangeSet?.name;
              if (data.changeSetId === this.selectedChangeSetId) {
                if (this.headChangeSetId) {
                  await this.setActiveChangeset(this.headChangeSetId);
                  toast({
                    component: MovedToHead,
                    props: {
                      icon: "trash",
                      changeSetName,
                      action: "abandoned",
                    },
                  });
                }
              }
              await this.FETCH_CHANGE_SETS();
            },
          },
          {
            eventType: "ChangeSetCancelled",
            callback: (data) => {
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
              const { changeSetId, userPk } = data as any as {
                changeSetId: string;
                userPk: UserId;
              };
              const changeSet = this.changeSetsById[changeSetId];
              if (changeSet) {
                changeSet.status = ChangeSetStatus.Abandoned;
                if (this.selectedChangeSet?.id === changeSetId) {
                  this.postAbandonActor = userPk;
                }
                this.changeSetsById[changeSetId] = changeSet;
              }

              this.FETCH_CHANGE_SETS();
            },
          },
          {
            eventType: "ChangeSetApplied",
            callback: (data) => {
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
              const { changeSetId, userPk, toRebaseChangeSetId } = data;
              const changeSet = this.changeSetsById[changeSetId];
              if (changeSet) {
                if (changeSet.id !== this.headChangeSetId)
                  // never set HEAD to Applied
                  changeSet.status = ChangeSetStatus.Applied;
                if (this.selectedChangeSet?.id === changeSetId) {
                  this.postApplyActor = userPk;
                }
                this.changeSetsById[changeSetId] = changeSet;
                // whenever the change set is applied move us to head
              }

              // TODO: jobelenus, I'm worried the WsEvent fires before commit happens
              /* if (this.selectedChangeSetId && !this.headSelected)
                this.FETCH_STATUS_WITH_BASE(this.selectedChangeSetId); */

              // did I get an update from head (and I am not head)?
              if (
                this.selectedChangeSetId === toRebaseChangeSetId &&
                this.selectedChangeSetId !== this.headChangeSetId
              ) {
                toast({
                  component: RebaseOnBase,
                });
              }

              // `list_open_change_sets` gets called prior on voters
              // which means the change set is gone, so always move
              if (
                !this.selectedChangeSetId ||
                this.selectedChangeSetId === changeSetId
              ) {
                const route = useRouterStore().currentRoute;

                if (route?.name) {
                  router.push({
                    name: route.name,
                    params: {
                      ...route.params,
                      changeSetId: "head",
                    },
                  });
                  if (
                    this.selectedChangeSet &&
                    this.selectedChangeSet.name !== "HEAD"
                  )
                    toast({
                      component: MovedToHead,
                      props: {
                        icon: "tools",
                        changeSetName: this.selectedChangeSet?.name,
                        action: "merged",
                      },
                    });
                }
              }
            },
          },
          {
            eventType: "ChangeSetBeginApprovalProcess",
            callback: (data) => {
              if (this.selectedChangeSet?.id === data.changeSetId) {
                this.changeSetApprovals = {};
              }
              const changeSet = this.changeSetsById[data.changeSetId];
              if (changeSet) {
                changeSet.status = ChangeSetStatus.NeedsApproval;
                changeSet.mergeRequestedAt = new Date().toISOString();
                changeSet.mergeRequestedByUserId = data.userPk;
              }
            },
          },
          {
            eventType: "ChangeSetCancelApprovalProcess",
            callback: (data) => {
              if (this.selectedChangeSet?.id === data.changeSetId) {
                this.changeSetApprovals = {};
              }
              const changeSet = this.changeSetsById[data.changeSetId];
              if (changeSet) {
                changeSet.status = ChangeSetStatus.Open;
              }
            },
          },

          {
            eventType: "ChangeSetBeginAbandonProcess",
            callback: (data) => {
              if (this.selectedChangeSet?.id === data.changeSetId) {
                this.changeSetApprovals = {};
              }
              const changeSet = this.changeSetsById[data.changeSetId];
              if (changeSet) {
                changeSet.status = ChangeSetStatus.NeedsAbandonApproval;
                changeSet.abandonRequestedAt = new Date().toISOString();
                changeSet.abandonRequestedByUserId = data.userPk;
              }
            },
          },
          {
            eventType: "ChangeSetCancelAbandonProcess",
            callback: (data) => {
              if (this.selectedChangeSet?.id === data.changeSetId) {
                this.changeSetApprovals = {};
              }
              const changeSet = this.changeSetsById[data.changeSetId];
              if (changeSet) {
                changeSet.status = ChangeSetStatus.Open;
              }
            },
          },
          {
            eventType: "ChangeSetMergeVote",
            callback: (data) => {
              if (this.selectedChangeSet?.id === data.changeSetId) {
                this.changeSetApprovals[data.userPk] = data.vote;
              }
            },
          },
          {
            eventType: "ChangeSetAbandonVote",
            callback: (data) => {
              if (this.selectedChangeSet?.id === data.changeSetId) {
                this.changeSetApprovals[data.userPk] = data.vote;
              }
            },
          },
          {
            eventType: "ChangeSetWritten",
            callback: (changeSetId) => {
              this.changeSetsWrittenAtById[changeSetId] = new Date();
            },
          },
        ]);

        const actionUnsub = this.$onAction(handleStoreError);

        return () => {
          actionUnsub();
          stopWatchSelectedChangeSet();
          realtimeStore.unsubscribe(this.$id);
        };
      },
    }),
  )();
}
