{
  stdenv,
  lib,
  runCommand,
  fetchurl,
  nix,
  linkFarm,
  includeGrammarIf ? _: true,
  grammarOverlays ? [],
  ...
}: let
  languagesConfig =
    builtins.fromTOML (builtins.readFile ./languages.toml);
  sourceCatalog = builtins.fromJSON (builtins.readFile ./grammar-sources/hashes.json);
  isGitGrammar = grammar:
    builtins.hasAttr "source" grammar
    && builtins.hasAttr "git" grammar.source
    && builtins.hasAttr "rev" grammar.source;
  allGitGrammars = builtins.filter isGitGrammar languagesConfig.grammar;
  missingSources = builtins.filter (grammar: !(sourceCatalog ? ${grammar.source.rev})) allGitGrammars;
  sourceFor = grammar: let
    pinned = sourceCatalog.${grammar.source.rev};
    repo = lib.removeSuffix ".git" (lib.removeSuffix "/" grammar.source.git);
    upstream =
      if lib.hasPrefix "https://gitlab.com/" repo
      then "https://gitlab.com/api/v4/projects/${lib.escapeURL (lib.removePrefix "https://gitlab.com/" repo)}/repository/archive.tar.gz?sha=${grammar.source.rev}"
      else "${repo}/archive/${grammar.source.rev}.tar.gz";
  in fetchurl {
    name = "source";
    hash = pinned.hash;
    urls = [pinned.url upstream];
    recursiveHash = true;
    downloadToTemp = true;
    # Nix preserves case-colliding paths on case-insensitive Darwin volumes.
    postFetch = if stdenv.buildPlatform.isDarwin then ''
      export GRAMMAR_ARCHIVE="file://$downloadedFile"
      export GRAMMAR_NAR_HASH=${lib.escapeShellArg pinned.hash}
      grammar_store_root="$TMPDIR/grammar-store"
      grammar_path=$(XDG_CACHE_HOME="$TMPDIR/grammar-cache" ${nix}/bin/nix \
        --extra-experimental-features 'nix-command flakes' \
        --store "local?root=$grammar_store_root" --option build-users-group "" \
        --option use-case-hack true --option substitute false --option substituters "" \
        eval --impure --raw --expr '(builtins.fetchTree {
          type = "tarball";
          url = builtins.getEnv "GRAMMAR_ARCHIVE";
          narHash = builtins.getEnv "GRAMMAR_NAR_HASH";
        }).outPath')
      mkdir "$out"
      touch "$out/.grammar-case-check"
      grammar_case_hack=false
      if test -e "$out/.GRAMMAR-CASE-CHECK"; then grammar_case_hack=true; fi
      rm "$out/.grammar-case-check"
      rmdir "$out"
      ${nix}/bin/nix-store --store "local?root=$grammar_store_root" --option use-case-hack true \
        --dump "$grammar_store_root$grammar_path" | \
        ${nix}/bin/nix-store --store "local?root=$grammar_store_root" \
          --option use-case-hack "$grammar_case_hack" --restore "$out"
    '' else ''
      mkdir "$out"
      tar -xzf "$downloadedFile" -C "$out" --strip-components=1
    '';
  };
  # If `use-grammars.only` is set, use only those grammars.
  # If `use-grammars.except` is set, use all other grammars.
  # Otherwise use all grammars.
  useGrammar = grammar:
    if languagesConfig ? use-grammars.only
    then builtins.elem grammar.name languagesConfig.use-grammars.only
    else if languagesConfig ? use-grammars.except
    then !(builtins.elem grammar.name languagesConfig.use-grammars.except)
    else true;
  grammarsToUse = builtins.filter useGrammar languagesConfig.grammar;
  gitGrammars = builtins.filter isGitGrammar grammarsToUse;
  buildGrammar = grammar:
    stdenv.mkDerivation {
      # see https://github.com/NixOS/nixpkgs/blob/fbdd1a7c0bc29af5325e0d7dd70e804a972eb465/pkgs/development/tools/parsing/tree-sitter/grammar.nix

      pname = "helix-tree-sitter-${grammar.name}";
      version = grammar.source.rev;

      src = sourceFor grammar;
      sourceRoot =
        if builtins.hasAttr "subpath" grammar.source
        then "source/${grammar.source.subpath}"
        else "source";

      dontConfigure = true;

      FLAGS = [
        "-Isrc"
        "-g"
        "-O3"
        "-fPIC"
        "-fno-exceptions"
        "-Wl,-z,relro,-z,now"
      ];

      SHARED_LIB = grammar.name + stdenv.hostPlatform.extensions.sharedLibrary;

      buildPhase = ''
        runHook preBuild

        if [[ -e src/scanner.cc ]]; then
          $CXX -c src/scanner.cc -o scanner.o $FLAGS
        elif [[ -e src/scanner.c ]]; then
          $CC -c src/scanner.c -o scanner.o $FLAGS
        fi

        $CC -c src/parser.c -o parser.o $FLAGS
        $CXX -shared -o $SHARED_LIB *.o

        runHook postBuild
      '';

      installPhase = ''
        runHook preInstall
        mkdir $out
        mv $SHARED_LIB $out/
        runHook postInstall
      '';

      # Strip failed on darwin: strip: error: symbols referenced by indirect symbol table entries that can't be stripped
      fixupPhase = lib.optionalString stdenv.isLinux ''
        runHook preFixup
        $STRIP $out/$SHARED_LIB
        runHook postFixup
      '';
    };
  grammarsToBuild = builtins.filter includeGrammarIf gitGrammars;
  builtGrammars =
    builtins.map (grammar: {
      inherit (grammar) name;
      value = buildGrammar grammar;
    })
    grammarsToBuild;
  extensibleGrammars =
    lib.makeExtensible (self: builtins.listToAttrs builtGrammars);
  overlaidGrammars =
    lib.pipe extensibleGrammars
    (builtins.map (overlay: grammar: grammar.extend overlay) grammarOverlays);
  sharedLibExtension = stdenv.hostPlatform.extensions.sharedLibrary;
  grammarLinks =
    lib.mapAttrsToList
    (name: artifact: "ln -s ${artifact}/${name}${sharedLibExtension} $out/${name}${sharedLibExtension}")
    (lib.filterAttrs (n: v: lib.isDerivation v) overlaidGrammars);
in
  assert lib.assertMsg (missingSources == [])
    "Missing pinned grammar sources: ${lib.concatMapStringsSep ", " (grammar: grammar.name) missingSources}; refresh grammar-sources/hashes.json.";
  runCommand "consolidated-helix-grammars" {
    passthru.sources = linkFarm "helix-grammar-sources" (map (grammar: {
      name = grammar.name;
      path = sourceFor grammar;
    }) allGitGrammars);
  } ''
    mkdir -p $out
    ${builtins.concatStringsSep "\n" grammarLinks}
  ''
