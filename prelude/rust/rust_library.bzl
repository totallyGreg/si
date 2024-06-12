# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is licensed under both the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree and the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree.

load(
    "@prelude//:artifact_tset.bzl",
    "ArtifactTSet",
    "make_artifact_tset",
)
load("@prelude//:resources.bzl", "ResourceInfo", "gather_resources")
load(
    "@prelude//android:android_providers.bzl",
    "merge_android_packageable_info",
)
load(
    "@prelude//cxx:linker.bzl",
    "PDB_SUB_TARGET",
    "get_pdb_providers",
)
load(
    "@prelude//cxx:omnibus.bzl",
    "create_linkable_root",
)
load(
    "@prelude//linking:link_groups.bzl",
    "merge_link_group_lib_info",
)
load(
    "@prelude//linking:link_info.bzl",
    "Archive",
    "ArchiveLinkable",
    "LibOutputStyle",
    "LinkInfo",
    "LinkInfos",
    "LinkStrategy",
    "LinkedObject",
    "MergedLinkInfo",  # @unused Used as a type
    "SharedLibLinkable",
    "create_merged_link_info",
    "create_merged_link_info_for_propagation",
    "get_lib_output_style",
    "legacy_output_style_to_link_style",
)
load(
    "@prelude//linking:linkable_graph.bzl",
    "DlopenableLibraryInfo",
    "LinkableGraph",  # @unused Used as a type
    "create_linkable_graph",
    "create_linkable_graph_node",
    "create_linkable_node",
)
load(
    "@prelude//linking:shared_libraries.bzl",
    "SharedLibraryInfo",  # @unused Used as a type
    "create_shared_libraries",
    "merge_shared_libraries",
)
load("@prelude//linking:types.bzl", "Linkage")
load("@prelude//os_lookup:defs.bzl", "OsLookup")
load(
    ":build.bzl",
    "compile_context",
    "generate_rustdoc",
    "generate_rustdoc_coverage",
    "generate_rustdoc_test",
    "rust_compile",
)
load(
    ":build_params.bzl",
    "BuildParams",  # @unused Used as a type
    "Emit",
    "LinkageLang",
    "MetadataKind",
    "RuleType",
    "build_params",
)
load(
    ":context.bzl",
    "CompileContext",  # @unused Used as a type
    "CrateName",  # @unused Used as a type
    "DepCollectionContext",
)
load(
    ":link_info.bzl",
    "DEFAULT_STATIC_LIB_OUTPUT_STYLE",
    "DEFAULT_STATIC_LINK_STRATEGY",
    "RustLinkInfo",
    "RustLinkStrategyInfo",
    "RustProcMacroMarker",  # @unused Used as a type
    "attr_crate",
    "attr_soname",
    "inherited_exported_link_deps",
    "inherited_link_group_lib_infos",
    "inherited_linkable_graphs",
    "inherited_merged_link_infos",
    "inherited_shared_libs",
    "resolve_deps",
    "resolve_rust_deps",
    "strategy_info",
)
load(
    ":outputs.bzl",
    "RustcExtraOutputsInfo",
    "RustcOutput",  # @unused Used as a type
    "output_as_diag_subtargets",
)
load(":proc_macro_alias.bzl", "rust_proc_macro_alias")
load(":resources.bzl", "rust_attr_resources")
load(":rust_toolchain.bzl", "RustToolchainInfo")
load(":targets.bzl", "targets")

def rust_library_impl(ctx: AnalysisContext) -> list[Provider]:
    compile_ctx = compile_context(ctx)
    toolchain_info = compile_ctx.toolchain_info

    # Multiple styles and language linkages could generate the same crate types
    # (eg procmacro or using preferred_linkage), so we need to see how many
    # distinct kinds of build we actually need to deal with.
    param_lang, lang_style_param = _build_params_for_styles(ctx, compile_ctx)

    # Grab the artifacts to use for the check subtargets. Picking a good
    # `LibOutputStyle` ensures that the subtarget shares work with the main
    # build if possible
    check_params = lang_style_param[(LinkageLang("rust"), DEFAULT_STATIC_LIB_OUTPUT_STYLE)]

    meta_fast = rust_compile(
        ctx = ctx,
        compile_ctx = compile_ctx,
        emit = Emit("metadata-fast"),
        params = check_params,
        default_roots = ["lib.rs"],
        designated_clippy = True,
    )

    # Generate the actions to build various output artifacts. Given the set of
    # parameters we need, populate maps to the linkable and metadata
    # artifacts by linkage lang.
    rust_param_artifact = {}
    native_param_artifact = {}
    for params, langs in param_lang.items():
        link = rust_compile(
            ctx = ctx,
            compile_ctx = compile_ctx,
            emit = Emit("link"),
            params = params,
            default_roots = ["lib.rs"],
        )

        if LinkageLang("rust") in langs:
            rust_param_artifact[params] = {
                MetadataKind("link"): link,
                MetadataKind("full"): rust_compile(
                    ctx = ctx,
                    compile_ctx = compile_ctx,
                    emit = Emit("metadata-full"),
                    params = params,
                    default_roots = ["lib.rs"],
                ),
                MetadataKind("fast"): meta_fast,
            }

        if LinkageLang("native") in langs or LinkageLang("native-unbundled") in langs:
            native_param_artifact[params] = link

    rust_artifacts = _rust_artifacts(
        ctx = ctx,
        compile_ctx = compile_ctx,
        lang_style_param = lang_style_param,
        rust_param_artifact = rust_param_artifact,
    )

    # For doctests, we need to know two things to know how to link them. The
    # first is that we need a link strategy, which affects how deps of this
    # target are handled
    if ctx.attrs.doc_link_style:
        doc_link_strategy = LinkStrategy(ctx.attrs.doc_link_style)
    else:
        # FIXME(JakobDegen): In this position, a binary would just fall back to
        # the default link style. However, we have a little bit of additional
        # information in the form of the preferred linkage that we can use to
        # make a different decision. There's nothing technically wrong with
        # that, but a comment explaining why we want to do it would be nice
        doc_link_strategy = {
            "any": LinkStrategy("shared"),
            "shared": LinkStrategy("shared"),
            "static": DEFAULT_STATIC_LINK_STRATEGY,
        }[ctx.attrs.preferred_linkage]

    # The second thing we need is a lib output style of the regular, non-doctest
    # version of this target that we want. Rustdoc does not handle this library
    # being built in a "shared" way well, so this must be a static output style.
    if ctx.attrs.doc_link_style:
        doc_output_style = {
            "shared": DEFAULT_STATIC_LIB_OUTPUT_STYLE,
            "static": LibOutputStyle("archive"),
            "static_pic": LibOutputStyle("pic_archive"),
        }[ctx.attrs.doc_link_style]
    else:
        doc_output_style = DEFAULT_STATIC_LIB_OUTPUT_STYLE
    static_library_params = lang_style_param[(LinkageLang("rust"), doc_output_style)]

    # Among {rustdoc, doctests, macro expand}, doctests are the only one which
    # cares about linkage. So whatever build params we picked for the doctests,
    # reuse them for the other two as well
    default_roots = ["lib.rs"]
    rustdoc = generate_rustdoc(
        ctx = ctx,
        compile_ctx = compile_ctx,
        params = static_library_params,
        default_roots = default_roots,
        document_private_items = False,
    )

    rustdoc_coverage = generate_rustdoc_coverage(
        ctx = ctx,
        compile_ctx = compile_ctx,
        params = static_library_params,
        default_roots = default_roots,
    )

    expand = rust_compile(
        ctx = ctx,
        compile_ctx = compile_ctx,
        emit = Emit("expand"),
        params = static_library_params,
        default_roots = default_roots,
    )

    # If doctests=True or False is set on the individual target, respect that.
    # Otherwise look at the global setting on the toolchain.
    doctests_enabled = \
        (ctx.attrs.doctests if ctx.attrs.doctests != None else toolchain_info.doctests) and \
        toolchain_info.rustc_target_triple == targets.exec_triple(ctx)

    rustdoc_test_params = build_params(
        rule = RuleType("binary"),
        proc_macro = ctx.attrs.proc_macro,
        link_strategy = doc_link_strategy,
        lib_output_style = None,
        lang = LinkageLang("rust"),
        linker_type = compile_ctx.cxx_toolchain_info.linker_info.type,
        target_os_type = ctx.attrs._target_os_type[OsLookup],
    )
    rustdoc_test = generate_rustdoc_test(
        ctx = ctx,
        compile_ctx = compile_ctx,
        link_strategy = rustdoc_test_params.dep_link_strategy,
        rlib = rust_param_artifact[static_library_params][MetadataKind("link")].output,
        params = rustdoc_test_params,
        default_roots = default_roots,
    )

    check_artifacts = rust_param_artifact[check_params]

    providers = []
    providers += _default_providers(
        lang_style_param = lang_style_param,
        rust_param_artifact = rust_param_artifact,
        native_param_artifact = native_param_artifact,
        rustdoc = rustdoc,
        rustdoc_test = rustdoc_test,
        doctests_enabled = doctests_enabled,
        check_artifacts = output_as_diag_subtargets(check_artifacts[MetadataKind("fast")]),
        expand = expand.output,
        sources = compile_ctx.symlinked_srcs,
        rustdoc_coverage = rustdoc_coverage,
    )
    providers += _rust_metadata_providers(
        check_artifacts = check_artifacts,
    )

    if ctx.attrs.proc_macro:
        providers += _proc_macro_link_providers(
            ctx = ctx,
            rust_artifacts = rust_artifacts,
        )
    else:
        link_infos = _link_infos(
            ctx = ctx,
            compile_ctx = compile_ctx,
            lang_style_param = lang_style_param,
            param_artifact = native_param_artifact,
        )

        if toolchain_info.advanced_unstable_linking:
            providers += _advanced_unstable_link_providers(
                ctx = ctx,
                compile_ctx = compile_ctx,
                lang_style_param = lang_style_param,
                rust_artifacts = rust_artifacts,
                native_param_artifact = native_param_artifact,
                link_infos = link_infos,
            )
        else:
            providers += _stable_link_providers(
                ctx = ctx,
                compile_ctx = compile_ctx,
                lang_style_param = lang_style_param,
                rust_artifacts = rust_artifacts,
                native_param_artifact = native_param_artifact,
                link_infos = link_infos,
            )

    deps = [dep.dep for dep in resolve_deps(ctx, compile_ctx.dep_ctx)]
    providers.append(ResourceInfo(resources = gather_resources(
        label = ctx.label,
        resources = rust_attr_resources(ctx),
        deps = deps,
    )))

    providers.append(merge_android_packageable_info(ctx.label, ctx.actions, deps))

    return providers

def _build_params_for_styles(
        ctx: AnalysisContext,
        compile_ctx: CompileContext) -> (
    dict[BuildParams, list[LinkageLang]],
    dict[(LinkageLang, LibOutputStyle), BuildParams],
):
    """
    For a given rule, return two things:
    - a set of build params we need for all combinations of linkage langages and
      link styles, mapped to which languages they apply to
    - a mapping from linkage language and link style to build params

    This is needed because different combinations may end up using the same set
    of params, and we want to minimize invocations to rustc, both for
    efficiency's sake, but also to avoid duplicate objects being linked
    together.
    """

    param_lang = {}  # param -> linkage_lang
    style_param = {}  # (linkage_lang, output_style) -> param

    target_os_type = ctx.attrs._target_os_type[OsLookup]
    linker_type = compile_ctx.cxx_toolchain_info.linker_info.type

    # Styles+lang linkage to params
    for linkage_lang in LinkageLang:
        # Skip proc_macro + non-rust combinations
        if ctx.attrs.proc_macro and linkage_lang != LinkageLang("rust"):
            continue

        for lib_output_style in LibOutputStyle:
            params = build_params(
                rule = RuleType("library"),
                proc_macro = ctx.attrs.proc_macro,
                link_strategy = None,
                lib_output_style = lib_output_style,
                lang = linkage_lang,
                linker_type = linker_type,
                target_os_type = target_os_type,
            )
            if params not in param_lang:
                param_lang[params] = []
            param_lang[params] = param_lang[params] + [linkage_lang]
            style_param[(linkage_lang, lib_output_style)] = params

    return (param_lang, style_param)

def _link_infos(
        ctx: AnalysisContext,
        compile_ctx: CompileContext,
        lang_style_param: dict[(LinkageLang, LibOutputStyle), BuildParams],
        param_artifact: dict[BuildParams, RustcOutput]) -> dict[LibOutputStyle, LinkInfos]:
    if ctx.attrs.proc_macro:
        # Don't need any of this for proc macros
        return {}

    advanced_unstable_linking = compile_ctx.toolchain_info.advanced_unstable_linking
    lang = LinkageLang("native-unbundled") if advanced_unstable_linking else LinkageLang("native")
    linker_type = compile_ctx.cxx_toolchain_info.linker_info.type

    link_infos = {}
    for output_style in LibOutputStyle:
        lib = param_artifact[lang_style_param[(lang, output_style)]]
        external_debug_info = make_artifact_tset(
            actions = ctx.actions,
            label = ctx.label,
            artifacts = filter(None, [lib.dwo_output_directory]),
            children = lib.extra_external_debug_info,
        )
        if output_style == LibOutputStyle("shared_lib"):
            link_infos[output_style] = LinkInfos(
                default = LinkInfo(
                    linkables = [SharedLibLinkable(lib = lib.output)],
                    external_debug_info = external_debug_info,
                ),
                stripped = LinkInfo(
                    linkables = [SharedLibLinkable(lib = lib.stripped_output)],
                    external_debug_info = external_debug_info,
                ),
            )
        else:
            link_infos[output_style] = LinkInfos(
                default = LinkInfo(
                    linkables = [ArchiveLinkable(
                        archive = Archive(artifact = lib.output),
                        linker_type = linker_type,
                    )],
                    external_debug_info = external_debug_info,
                ),
                stripped = LinkInfo(
                    linkables = [ArchiveLinkable(
                        archive = Archive(artifact = lib.stripped_output),
                        linker_type = linker_type,
                    )],
                ),
            )
    return link_infos

def _rust_artifacts(
        ctx: AnalysisContext,
        compile_ctx: CompileContext,
        lang_style_param: dict[(LinkageLang, LibOutputStyle), BuildParams],
        rust_param_artifact: dict[BuildParams, dict[MetadataKind, RustcOutput]]) -> dict[LinkStrategy, RustLinkStrategyInfo]:
    pic_behavior = compile_ctx.cxx_toolchain_info.pic_behavior
    preferred_linkage = Linkage(ctx.attrs.preferred_linkage)

    rust_artifacts = {}
    for link_strategy in LinkStrategy:
        params = lang_style_param[(LinkageLang("rust"), get_lib_output_style(link_strategy, preferred_linkage, pic_behavior))]
        rust_artifacts[link_strategy] = _handle_rust_artifact(ctx, compile_ctx.dep_ctx, link_strategy, rust_param_artifact[params])
    return rust_artifacts

def _handle_rust_artifact(
        ctx: AnalysisContext,
        dep_ctx: DepCollectionContext,
        link_strategy: LinkStrategy,
        outputs: dict[MetadataKind, RustcOutput]) -> RustLinkStrategyInfo:
    """
    Return the RustLinkStrategyInfo for a given set of artifacts. The main consideration
    is computing the right set of dependencies.
    """

    # If we're a crate where our consumers should care about transitive deps,
    # then compute them (specifically, not proc-macro).
    link_output = outputs[MetadataKind("link")]
    if not ctx.attrs.proc_macro:
        tdeps, external_debug_info, tprocmacrodeps = _compute_transitive_deps(ctx, dep_ctx, link_strategy)
        external_debug_info = make_artifact_tset(
            actions = ctx.actions,
            label = ctx.label,
            artifacts = filter(None, [link_output.dwo_output_directory]),
            children = external_debug_info,
        )
        return RustLinkStrategyInfo(
            outputs = {m: x.output for m, x in outputs.items()},
            transitive_deps = tdeps,
            transitive_proc_macro_deps = tprocmacrodeps,
            pdb = link_output.pdb,
            external_debug_info = external_debug_info,
        )
    else:
        # Proc macro deps are always the real thing
        return RustLinkStrategyInfo(
            outputs = {m: link_output.output for m in MetadataKind},
            transitive_deps = {m: {} for m in MetadataKind},
            transitive_proc_macro_deps = {},
            pdb = link_output.pdb,
            external_debug_info = ArtifactTSet(),
        )

def _default_providers(
        lang_style_param: dict[(LinkageLang, LibOutputStyle), BuildParams],
        rust_param_artifact: dict[BuildParams, dict[MetadataKind, RustcOutput]],
        native_param_artifact: dict[BuildParams, RustcOutput],
        rustdoc: Artifact,
        rustdoc_test: cmd_args,
        doctests_enabled: bool,
        check_artifacts: dict[str, Artifact | None],
        expand: Artifact,
        sources: Artifact,
        rustdoc_coverage: Artifact) -> list[Provider]:
    targets = {}
    targets.update(check_artifacts)
    targets["sources"] = sources
    targets["expand"] = expand
    targets["doc"] = rustdoc
    targets["doc-coverage"] = rustdoc_coverage
    sub_targets = {
        k: [DefaultInfo(default_output = v)]
        for (k, v) in targets.items()
    }

    # Add provider for default output, and for each lib output style...
    # FIXME(JakobDegen): C++ rules only provide some of the output styles,
    # determined by `get_output_styles_for_linkage` in `linking/link_info.bzl`.
    # Do we want to do the same?
    for output_style in LibOutputStyle:
        link = rust_param_artifact[lang_style_param[(LinkageLang("rust"), output_style)]][MetadataKind("link")]
        nested_sub_targets = {}
        if link.pdb:
            nested_sub_targets[PDB_SUB_TARGET] = get_pdb_providers(pdb = link.pdb, binary = link.output)

        # FIXME(JakobDegen): Ideally we'd use the same
        # `subtarget_for_output_style` as C++, but that uses `static-pic`
        # instead of `static_pic`. Would be nice if that were consistent
        name = legacy_output_style_to_link_style(output_style).value
        sub_targets[name] = [DefaultInfo(
            default_output = link.output,
            sub_targets = nested_sub_targets,
        )]

    lang_style_for_staticlib = (LinkageLang("native"), LibOutputStyle("archive"))
    if lang_style_for_staticlib in lang_style_param:
        artifact = native_param_artifact[lang_style_param[lang_style_for_staticlib]]
        sub_targets["staticlib"] = [DefaultInfo(
            default_output = artifact.output,
        )]

    lang_style_for_cdylib = (LinkageLang("native"), LibOutputStyle("shared_lib"))
    if lang_style_for_cdylib in lang_style_param:
        artifact = native_param_artifact[lang_style_param[lang_style_for_cdylib]]
        sub_targets["cdylib"] = [DefaultInfo(
            default_output = artifact.output,
        )]

    providers = []

    rustdoc_test_info = ExternalRunnerTestInfo(
        type = "rustdoc",
        command = [rustdoc_test],
        run_from_project_root = True,
    )

    # Always let the user run doctests via `buck2 test :crate[doc]`
    sub_targets["doc"].append(rustdoc_test_info)

    # But only run it as a part of `buck2 test :crate` if it's not disabled
    if doctests_enabled:
        providers.append(rustdoc_test_info)

    providers.append(DefaultInfo(
        default_output = check_artifacts["check"],
        sub_targets = sub_targets,
    ))

    return providers

def _rust_metadata_providers(check_artifacts: dict[MetadataKind, RustcOutput]) -> list[Provider]:
    return [
        RustcExtraOutputsInfo(
            metadata_full = check_artifacts[MetadataKind("full")],
            metadata_fast = check_artifacts[MetadataKind("fast")],
        ),
    ]

def _proc_macro_link_providers(
        ctx: AnalysisContext,
        rust_artifacts: dict[LinkStrategy, RustLinkStrategyInfo]) -> list[Provider]:
    # These are never accessed in the case of proc macros, so just return some dummy
    # values
    return [RustLinkInfo(
        crate = attr_crate(ctx),
        strategies = rust_artifacts,
        merged_link_info = create_merged_link_info_for_propagation(ctx, []),
        exported_link_deps = [],
        shared_libs = merge_shared_libraries(ctx.actions),
        linkable_graphs = [],
    )]

def _advanced_unstable_link_providers(
        ctx: AnalysisContext,
        compile_ctx: CompileContext,
        lang_style_param: dict[(LinkageLang, LibOutputStyle), BuildParams],
        rust_artifacts: dict[LinkStrategy, RustLinkStrategyInfo],
        native_param_artifact: dict[BuildParams, RustcOutput],
        link_infos: dict[LibOutputStyle, LinkInfos]) -> list[Provider]:
    crate = attr_crate(ctx)
    pic_behavior = compile_ctx.cxx_toolchain_info.pic_behavior
    preferred_linkage = Linkage(ctx.attrs.preferred_linkage)

    providers = []

    dep_ctx = compile_ctx.dep_ctx
    linker_info = compile_ctx.cxx_toolchain_info.linker_info

    inherited_link_infos = inherited_merged_link_infos(ctx, dep_ctx)
    inherited_shlibs = inherited_shared_libs(ctx, dep_ctx)
    inherited_graphs = inherited_linkable_graphs(ctx, dep_ctx)
    inherited_exported_deps = inherited_exported_link_deps(ctx, dep_ctx)

    # Native link provider.
    merged_link_info = create_merged_link_info(
        ctx,
        pic_behavior,
        link_infos,
        deps = inherited_link_infos,
        exported_deps = filter(None, [d.get(MergedLinkInfo) for d in inherited_exported_deps]),
        preferred_linkage = preferred_linkage,
    )
    providers.append(merged_link_info)

    solibs = {}

    # Add the shared library to the list of shared libs.
    shlib_name = attr_soname(ctx)

    shared_lib_params = lang_style_param[(LinkageLang("native-unbundled"), LibOutputStyle("shared_lib"))]
    shared_lib_output = native_param_artifact[shared_lib_params].output

    # Only add a shared library if we generated one.
    # TODO(cjhopman): This is strange. Normally (like in c++) the link_infos passed to create_merged_link_info above would only have
    # a value for LibOutputStyle("shared_lib") if that were created and we could just check for that key. Given that I intend
    # to remove the SharedLibraries provider, maybe just wait for that to resolve this.
    if get_lib_output_style(LinkStrategy("shared"), preferred_linkage, compile_ctx.cxx_toolchain_info.pic_behavior) == LibOutputStyle("shared_lib"):
        solibs[shlib_name] = LinkedObject(
            output = shared_lib_output,
            unstripped_output = shared_lib_output,
            external_debug_info = link_infos[LibOutputStyle("shared_lib")].default.external_debug_info,
        )

    # Native shared library provider.
    shared_libs = create_shared_libraries(ctx, solibs)
    shared_library_info = merge_shared_libraries(
        ctx.actions,
        shared_libs,
        inherited_shlibs,
    )
    providers.append(shared_library_info)

    linkable_graph = create_linkable_graph(
        ctx,
        node = create_linkable_graph_node(
            ctx,
            linkable_node = create_linkable_node(
                ctx = ctx,
                preferred_linkage = preferred_linkage,
                deps = inherited_graphs,
                exported_deps = inherited_exported_deps,
                link_infos = link_infos,
                shared_libs = shared_libs,
                default_soname = shlib_name,
                # Link groups have a heuristic in which they assume that a
                # preferred_linkage = "static" library needs to be linked
                # into every single link group, instead of just one.
                # Applying that same heuristic to Rust seems right, but only
                # if this target actually requested that. Opt ourselves out
                # if it didn't.
                ignore_force_static_follows_dependents = preferred_linkage != Linkage("static"),
                include_in_android_mergemap = False,  # TODO(pickett): Plumb D54748362 to the macro layer
            ),
        ),
        deps = inherited_graphs + inherited_exported_deps,
    )

    providers.append(linkable_graph)

    # Omnibus root provider.
    linkable_root = create_linkable_root(
        label = ctx.label,
        name = shlib_name,
        link_infos = LinkInfos(
            default = LinkInfo(
                linkables = [ArchiveLinkable(
                    archive = Archive(
                        artifact = shared_lib_output,
                    ),
                    linker_type = linker_info.type,
                    link_whole = True,
                )],
                external_debug_info = link_infos[LibOutputStyle("pic_archive")].default.external_debug_info,
            ),
        ),
        deps = inherited_graphs,
    )
    providers.append(linkable_root)

    # Mark libraries that support `dlopen`.
    if getattr(ctx.attrs, "supports_python_dlopen", False):
        providers.append(DlopenableLibraryInfo())

    # We never need to add anything to this provider because Rust libraries
    # cannot act as link group libs, especially given that they only support
    # auto link groups anyway
    providers.append(merge_link_group_lib_info(children = inherited_link_group_lib_infos(ctx, compile_ctx.dep_ctx)))

    # Create rust library provider.
    providers.append(RustLinkInfo(
        crate = crate,
        strategies = rust_artifacts,
        merged_link_info = merged_link_info,
        exported_link_deps = inherited_exported_deps,
        shared_libs = shared_library_info,
        # We've already reported transitive deps on the inherited graphs, so for
        # most purposes it would be fine to just have `linkable_graph` here.
        # However, link groups do an analysis that relies on each symbol
        # reference having a matching edge in the link graph, and so reexports
        # and generics mean that we have to report a dependency on all
        # transitive Rust deps and their immediate non-Rust deps
        linkable_graphs = inherited_graphs + [linkable_graph],
    ))

    return providers

def _stable_link_providers(
        ctx: AnalysisContext,
        compile_ctx: CompileContext,
        lang_style_param: dict[(LinkageLang, LibOutputStyle), BuildParams],
        native_param_artifact: dict[BuildParams, RustcOutput],
        rust_artifacts: dict[LinkStrategy, RustLinkStrategyInfo],
        link_infos: dict[LibOutputStyle, LinkInfos]) -> list[Provider]:
    providers = []

    crate = attr_crate(ctx)

    merged_link_info, shared_libs, linkable_graphs, exported_link_deps = _rust_link_providers(ctx, compile_ctx.dep_ctx)

    # Create rust library provider.
    rust_link_info = RustLinkInfo(
        crate = crate,
        strategies = rust_artifacts,
        merged_link_info = merged_link_info,
        exported_link_deps = exported_link_deps,
        shared_libs = shared_libs,
        linkable_graphs = linkable_graphs,
    )

    providers.append(rust_link_info)
    providers += _native_link_providers(ctx, compile_ctx, lang_style_param, native_param_artifact, link_infos, rust_link_info)
    return providers

def _rust_link_providers(
        ctx: AnalysisContext,
        dep_ctx: DepCollectionContext) -> (
    MergedLinkInfo,
    SharedLibraryInfo,
    list[LinkableGraph],
    list[Dependency],
):
    inherited_link_infos = inherited_merged_link_infos(ctx, dep_ctx)
    inherited_shlibs = inherited_shared_libs(ctx, dep_ctx)
    inherited_graphs = inherited_linkable_graphs(ctx, dep_ctx)
    inherited_exported_deps = inherited_exported_link_deps(ctx, dep_ctx)

    merged_link_info = create_merged_link_info_for_propagation(ctx, inherited_link_infos)
    shared_libs = merge_shared_libraries(
        ctx.actions,
        deps = inherited_shlibs,
    )
    return (merged_link_info, shared_libs, inherited_graphs, inherited_exported_deps)

def _native_link_providers(
        ctx: AnalysisContext,
        compile_ctx: CompileContext,
        lang_style_param: dict[(LinkageLang, LibOutputStyle), BuildParams],
        param_artifact: dict[BuildParams, RustcOutput],
        link_infos: dict[LibOutputStyle, LinkInfos],
        rust_link_info: RustLinkInfo) -> list[Provider]:
    """
    Return the set of providers needed to link Rust as a dependency for native
    (ie C/C++) code, along with relevant dependencies.
    """

    # We collected transitive deps in the Rust link providers
    inherited_link_infos = [rust_link_info.merged_link_info]
    inherited_shlibs = [rust_link_info.shared_libs]
    inherited_link_graphs = rust_link_info.linkable_graphs
    inherited_exported_deps = rust_link_info.exported_link_deps

    linker_info = compile_ctx.cxx_toolchain_info.linker_info
    linker_type = linker_info.type

    providers = []

    shared_lib_params = lang_style_param[(LinkageLang("native"), LibOutputStyle("shared_lib"))]
    shared_lib_output = param_artifact[shared_lib_params].output

    preferred_linkage = Linkage(ctx.attrs.preferred_linkage)

    # Native link provider.
    providers.append(create_merged_link_info(
        ctx,
        compile_ctx.cxx_toolchain_info.pic_behavior,
        link_infos,
        deps = inherited_link_infos,
        exported_deps = filter(None, [d.get(MergedLinkInfo) for d in inherited_exported_deps]),
        preferred_linkage = preferred_linkage,
    ))

    solibs = {}

    # Add the shared library to the list of shared libs.
    shlib_name = attr_soname(ctx)

    # Only add a shared library if we generated one.
    # TODO(cjhopman): This is strange. Normally (like in c++) the link_infos passed to create_merged_link_info above would only have
    # a value for LibOutputStyle("shared_lib") if that were created and we could just check for that key. Given that I intend
    # to remove the SharedLibraries provider, maybe just wait for that to resolve this.
    if get_lib_output_style(LinkStrategy("shared"), preferred_linkage, compile_ctx.cxx_toolchain_info.pic_behavior) == LibOutputStyle("shared_lib"):
        solibs[shlib_name] = LinkedObject(
            output = shared_lib_output,
            unstripped_output = shared_lib_output,
            external_debug_info = link_infos[LibOutputStyle("shared_lib")].default.external_debug_info,
        )

    # Native shared library provider.
    shared_libs = create_shared_libraries(ctx, solibs)
    providers.append(merge_shared_libraries(
        ctx.actions,
        shared_libs,
        inherited_shlibs,
    ))

    # Omnibus root provider.
    linkable_root = create_linkable_root(
        label = ctx.label,
        name = shlib_name,
        link_infos = LinkInfos(
            default = LinkInfo(
                linkables = [ArchiveLinkable(
                    archive = Archive(
                        artifact = shared_lib_output,
                    ),
                    linker_type = linker_type,
                    link_whole = True,
                )],
                external_debug_info = link_infos[LibOutputStyle("pic_archive")].default.external_debug_info,
            ),
        ),
        deps = inherited_link_graphs,
    )
    providers.append(linkable_root)

    # Mark libraries that support `dlopen`.
    if getattr(ctx.attrs, "supports_python_dlopen", False):
        providers.append(DlopenableLibraryInfo())

    linkable_graph = create_linkable_graph(
        ctx,
        node = create_linkable_graph_node(
            ctx,
            linkable_node = create_linkable_node(
                ctx = ctx,
                preferred_linkage = preferred_linkage,
                deps = inherited_link_graphs,
                exported_deps = inherited_exported_deps,
                link_infos = link_infos,
                shared_libs = shared_libs,
                default_soname = shlib_name,
                include_in_android_mergemap = False,
            ),
        ),
        deps = inherited_link_graphs + inherited_exported_deps,
    )

    providers.append(linkable_graph)

    # We never need to add anything to this provider because Rust libraries
    # cannot act as link group libs, especially given that they only support
    # auto link groups anyway
    providers.append(merge_link_group_lib_info(children = inherited_link_group_lib_infos(ctx, compile_ctx.dep_ctx)))

    return providers

# Compute transitive deps. Caller decides whether this is necessary.
def _compute_transitive_deps(
        ctx: AnalysisContext,
        dep_ctx: DepCollectionContext,
        dep_link_strategy: LinkStrategy) -> (
    dict[MetadataKind, dict[Artifact, CrateName]],
    list[ArtifactTSet],
    dict[RustProcMacroMarker, ()],
):
    toolchain_info = ctx.attrs._rust_toolchain[RustToolchainInfo]
    transitive_deps = {m: {} for m in MetadataKind}
    external_debug_info = []
    transitive_proc_macro_deps = {}

    for dep in resolve_rust_deps(ctx, dep_ctx):
        if dep.proc_macro_marker != None:
            transitive_proc_macro_deps[dep.proc_macro_marker] = ()

            # We don't want to propagate proc macros directly, and they have no transitive deps
            continue
        strategy = strategy_info(toolchain_info, dep.info, dep_link_strategy)
        for m in MetadataKind:
            transitive_deps[m][strategy.outputs[m]] = dep.info.crate
            transitive_deps[m].update(strategy.transitive_deps[m])

        external_debug_info.append(strategy.external_debug_info)

        transitive_proc_macro_deps.update(strategy.transitive_proc_macro_deps)

    return transitive_deps, external_debug_info, transitive_proc_macro_deps

def rust_library_macro_wrapper(rust_library: typing.Callable) -> typing.Callable:
    def wrapper(**kwargs):
        if not kwargs.pop("_use_legacy_proc_macros", False) and kwargs.get("proc_macro") == True:
            name = kwargs["name"]
            if kwargs.get("crate", None) == None and kwargs.get("crate_dynamic", None) == None:
                kwargs["crate"] = name.replace("-", "_")

            rust_proc_macro_alias(
                name = name,
                actual_exec = ":_" + name,
                actual_plugin = ":_" + name,
                visibility = kwargs.pop("visibility", []),
            )
            kwargs["name"] = "_" + name

        rust_library(**kwargs)

    return wrapper
