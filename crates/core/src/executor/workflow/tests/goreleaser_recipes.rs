// ---
// relationships:
//   implements: github-release-executor
// ---

// Executed GoReleaser publisher recipes moved from `executor::workflow::tests`.

    /// The maintained GoReleaser recipes, executed against stubbed remotes.
    ///
    /// A promotion body is shell, and what this task is accountable for -- the
    /// files that land at the destination, under the names the destination
    /// requires, from the layout the packager actually writes -- is a property
    /// of what that shell does rather than of what it says. Asserting the
    /// emitted text restates the recipe; it cannot notice that the packager
    /// writes `aur/<name>.pkgbuild` where the recipe looked for `PKGBUILD`.
    ///
    /// So each scenario runs the derived body over a fixture shaped like the
    /// packager's real distribution tree, with the remote-reaching commands
    /// replaced by stubs, and reads back what arrived at the destination.
    /// `git` is not stubbed away: the stub only rewrites the destination URL to
    /// a local bare repository and hands the invocation to the real client, so
    /// clone, add, status, commit, and push keep their own semantics.
    pub(super) mod goreleaser_recipes {
        use super::*;
        use crate::executor::fixture::Workspace;
        use std::path::PathBuf;

        const HOMEBREW_JOB: &str = "intentional_publish_component_package_homebrew_primary";
        const AUR_JOB: &str = "intentional_publish_component_package_aur_primary";
        /// Package identity the Arch destination resolves, from native evidence.
        const AUR_PACKAGE: &str = "example-tool-bin";

        /// Native configuration declaring two taps and two Arch packages.
        ///
        /// One `brews` entry keeps the packager's default directory and the
        /// other declares its own, because the recipe's claim is that the
        /// native member decides where a formula lands. The second `aur` entry
        /// exists to be left alone: its files share one directory with this
        /// publication's, and promoting them would publish another package's
        /// sources under this package's name.
        ///
        /// The `aur` names are declared the way a repository declares them,
        /// without the suffix the packager adds. Pre-normalising them here
        /// would make the identity the derivation produces and the file the
        /// packager writes agree by construction, and no scenario could then
        /// observe them disagreeing -- which is the shape a real defect took.
        const NATIVE_CONFIG: &str = r#"version: 2
project_name: example-tool
builds:
  - main: ./cmd/example-tool
brews:
  - repository: { owner: example-org, name: homebrew-tap }
  - repository: { owner: example-org, name: homebrew-tap }
    directory: HomebrewFormula
nfpms:
  - formats: [ rpm, deb ]
aur:
  - name: example-tool
  - name: example-other
"#;

        /// The same release unit, with its first Arch entry unnamed.
        ///
        /// The packager resolves an unnamed entry to the project name, so this
        /// declares the same destination a different way, and it is the shape
        /// in which a dropped entry would silently promote the sibling.
        const UNNAMED_ARCH_CONFIG: &str = r#"version: 2
project_name: example-tool
builds:
  - main: ./cmd/example-tool
aur:
  - {}
  - name: example-other
"#;

        /// One derived promotion body and the destinations it can reach.
        struct Recipe {
            workspace: Workspace,
            remotes: PathBuf,
            stubs: PathBuf,
            temp: PathBuf,
            job: String,
        }

        /// What running one promotion body produced.
        struct Outcome {
            status: std::process::ExitStatus,
            stderr: String,
        }

        impl Outcome {
            fn succeeded(&self) -> bool {
                self.status.success()
            }

            fn expect_success(&self) -> &Self {
                assert!(
                    self.succeeded(),
                    "the promotion body succeeds: {}",
                    self.stderr
                );
                self
            }
        }

        impl Recipe {
            fn new(label: &str, job: &str) -> Self {
                Self::declaring(label, job, NATIVE_CONFIG)
            }

            fn declaring(label: &str, job: &str, native: &str) -> Self {
                let workspace = go_workspace(label);
                workspace.write("component/.goreleaser.yaml", native);
                converge(workspace.root(), WorkflowRole::Publish);
                Self::from_workspace(workspace, job)
            }

            fn from_workspace(workspace: Workspace, job: &str) -> Self {
                let root = workspace.root().to_path_buf();
                let remotes = root.join("remotes");
                let stubs = root.join("stubs");
                std::fs::create_dir_all(&remotes).expect("remote directory");
                std::fs::create_dir_all(&stubs).expect("stub directory");
                write_stubs(&stubs);
                Self {
                    temp: root.join("runner-temp"),
                    workspace,
                    remotes,
                    stubs,
                    job: job.to_owned(),
                }
            }

            /// Create the destination repository this publication pushes into.
            fn with_destination(self, name: &str) -> Self {
                let path = self.remotes.join(name);
                let status = std::process::Command::new("git")
                    .args(["init", "--quiet", "--bare"])
                    .arg(&path)
                    .status()
                    .expect("git init runs");
                assert!(status.success(), "the destination repository exists");
                let status = std::process::Command::new("git")
                    .args([
                        "-C",
                        path.to_str().expect("destination path"),
                        "symbolic-ref",
                    ])
                    .args(["HEAD", "refs/heads/master"])
                    .status()
                    .expect("git symbolic-ref runs");
                assert!(status.success(), "the destination branch is master");
                self
            }

            /// Write the distribution tree the build job would have produced.
            ///
            /// The layout below is GoReleaser's own, at the release
            /// [`super::templates::GORELEASER_VERSION`] pins: `homebrew/<directory>/<name>.rb` and
            /// `aur/<package>.pkgbuild`. Every promotion these scenarios execute
            /// reads those paths, so raising that pin is a change to what this
            /// fixture asserts and both move together or the scenarios go on
            /// passing against a layout the packager no longer writes.
            fn with_distribution(self) -> Self {
                let bytes = self.temp.join("intentional_subject/bytes");
                for (relative, contents) in [
                    (
                        "homebrew/Formula/example-tool.rb",
                        "class ExampleTool < Formula\nend\n",
                    ),
                    (
                        "homebrew/HomebrewFormula/example-tool.rb",
                        "class ExampleTool < Formula\nend\n",
                    ),
                    // A cask is generated Ruby that is not a formula, and it
                    // sorts before the formulas the tap expects.
                    (
                        "homebrew_casks/example-tool.rb",
                        "cask 'example-tool' do\nend\n",
                    ),
                    (
                        "aur/example-tool-bin.pkgbuild",
                        "pkgname=example-tool-bin\n",
                    ),
                    (
                        "aur/example-tool-bin.srcinfo",
                        "pkgbase = example-tool-bin\n",
                    ),
                    (
                        "aur/example-other-bin.pkgbuild",
                        "pkgname=example-other-bin\n",
                    ),
                    (
                        "aur/example-other-bin.srcinfo",
                        "pkgbase = example-other-bin\n",
                    ),
                    ("example-tool_1.0.0_amd64.deb", "the deb the release built"),
                ] {
                    let path = bytes.join(relative);
                    std::fs::create_dir_all(path.parent().expect("parent"))
                        .expect("dist directory");
                    std::fs::write(path, contents).expect("dist file");
                }
                self
            }

            /// Run the derived promotion body, optionally with a drifted host key.
            fn run(&self) -> Outcome {
                self.run_with_host_fingerprint(None)
            }

            fn run_with_host_fingerprint(&self, fingerprint: Option<&str>) -> Outcome {
                let root = self.workspace.root();
                let step = publish_step(root, &self.job, &self.temp);
                let pinned = step
                    .env
                    .get("INTENTIONAL_AUR_HOST_FINGERPRINT")
                    .cloned()
                    .unwrap_or_default();
                let mut command = std::process::Command::new("bash");
                command
                    .arg("-c")
                    .arg(&step.run)
                    .current_dir(root)
                    .env_clear()
                    .env(
                        "PATH",
                        format!(
                            "{}:{}",
                            self.stubs.display(),
                            test_tool_path(&std::env::var("PATH").unwrap_or_default())
                        ),
                    )
                    .env("HOME", root)
                    .env("RUNNER_TEMP", &self.temp)
                    .env("FAKE_REMOTES", &self.remotes)
                    // The stub reports whatever fingerprint the scenario says
                    // the host answered with, so the recipe's comparison against
                    // its own pinned value is what decides the outcome.
                    .env("FAKE_HOST_FINGERPRINT", fingerprint.unwrap_or(&pinned));
                for (name, value) in &step.env {
                    command.env(name, value);
                }
                let output = command.output().expect("the promotion body runs");
                Outcome {
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                }
            }

            /// Files the destination repository carries, by repository path.
            fn destination_files(&self, name: &str) -> BTreeMap<String, String> {
                let checkout = self.workspace.root().join(format!("read-{name}"));
                let _ = std::fs::remove_dir_all(&checkout);
                let status = std::process::Command::new("git")
                    .arg("clone")
                    .arg("--quiet")
                    .arg(self.remotes.join(name))
                    .arg(&checkout)
                    .status()
                    .expect("git clone runs");
                assert!(status.success(), "the destination repository is readable");
                let mut files = BTreeMap::new();
                collect(&checkout, &checkout, &mut files);
                files
            }

            /// Commits the destination repository carries.
            fn destination_commits(&self, name: &str) -> usize {
                let output = std::process::Command::new("git")
                    .arg("-C")
                    .arg(self.remotes.join(name))
                    .args(["rev-list", "--count", "--all"])
                    .output()
                    .expect("git rev-list runs");
                String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse()
                    .unwrap_or(0)
            }

            /// Whether the destination repository exists at all.
            fn destination_exists(&self, name: &str) -> bool {
                self.remotes.join(name).is_dir()
            }
        }

        /// Every tracked file beneath one checkout, by repository-relative path.
        fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(directory).expect("checkout readable") {
                let path = entry.expect("checkout entry").path();
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                if path.is_dir() {
                    collect(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root)
                            .expect("relative")
                            .display()
                            .to_string(),
                        std::fs::read_to_string(&path).expect("file contents"),
                    );
                }
            }
        }

        struct PublishStep {
            run: String,
            env: BTreeMap<String, String>,
        }

        /// Read the derived publish step rather than restating it.
        ///
        /// The environment comes from the step's own `env:` mapping, so a value
        /// the derivation stopped routing, or started spelling differently, is
        /// absent here rather than supplied by the test.
        fn publish_step(root: &Path, job: &str, temp: &Path) -> PublishStep {
            let document: Value = serde_yaml::from_str(&workflow(root, WorkflowRole::Publish))
                .expect("workflow parses");
            let steps = document["jobs"][job]["steps"]
                .as_sequence()
                .unwrap_or_else(|| panic!("{job} has steps"));
            let step = steps
                .iter()
                .find(|step| {
                    step["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("Publish "))
                })
                .unwrap_or_else(|| panic!("{job} has a publish step"));
            let env = step["env"]
                .as_mapping()
                .expect("the publish step routes its values through env")
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().expect("env name").to_owned(),
                        expression(value.as_str().expect("env value"), temp),
                    )
                })
                .collect();
            PublishStep {
                run: step["run"].as_str().expect("publish body").to_owned(),
                env,
            }
        }

        /// Stand in for the workflow expressions a runner would have resolved.
        fn expression(value: &str, temp: &Path) -> String {
            if value.contains("steps.") && value.contains("token") {
                return "a-short-lived-installation-token".to_owned();
            }
            if value.contains("secrets.") {
                return "a-repository-supplied-secret".to_owned();
            }
            value
                .replace("${{ runner.temp }}", &temp.display().to_string())
                .replace("${{ github.ref_name }}", "component/staged@1.0.0")
        }

        fn write_stubs(directory: &Path) {
            let git = which("git");
            for (name, body) in [
                ("git", GIT_STUB.replace("@GIT@", &git)),
                ("ssh-keyscan", SSH_KEYSCAN_STUB.to_owned()),
                ("ssh-keygen", SSH_KEYGEN_STUB.to_owned()),
            ] {
                let path = directory.join(name);
                std::fs::write(&path, body).expect("stub written");
                let status = std::process::Command::new("chmod")
                    .arg("+x")
                    .arg(&path)
                    .status()
                    .expect("chmod runs");
                assert!(status.success(), "{name} is executable");
            }
        }

        /// Absolute path of one command, so a stub can delegate to the real one.
        fn which(command: &str) -> String {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {command}"))
                .output()
                .expect("command lookup runs");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }

        /// Rewrite a remote destination to a local repository and run real git.
        ///
        /// Only the URL is stubbed. Everything the recipe depends on -- that a
        /// clone of an unregistered package fails, that `status --porcelain` is
        /// empty when nothing changed, that a push lands what was committed --
        /// is the real client's behaviour rather than a stub's imitation.
        const GIT_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
arguments=()
for argument in "$@"; do
  case "${argument}" in
    https://*@github.com/*|ssh://aur@aur.archlinux.org/*)
      name="${argument##*/}"
      resolved="${FAKE_REMOTES}/${name%.git}"
      # The Arch User Repository registers a package on its initial push, so a
      # destination the recipe adds as a remote is one a push would create. A
      # clone still fails against an absent package, which is the branch the
      # recipe has to handle.
      if [[ " $* " == *" remote "* ]] && [ ! -d "${resolved}" ]; then
        @GIT@ init --quiet --bare "${resolved}"
        @GIT@ -C "${resolved}" symbolic-ref HEAD refs/heads/master
      fi
      arguments+=("${resolved}")
      ;;
    *) arguments+=("${argument}") ;;
  esac
done
exec @GIT@ "${arguments[@]}"
"#;

        const SSH_KEYSCAN_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s ssh-ed25519 %s\n' "${*: -1}" "the key the host answered with"
"#;

        const SSH_KEYGEN_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '256 %s host (ED25519)\n' "${FAKE_HOST_FINGERPRINT}"
"#;

        // The packager writes each formula under the directory its own `brews`
        // entry declares and publishes it at that same path. Choosing one file
        // out of the tree, or choosing the directory here, both publish a tap
        // the native configuration did not describe.
        #[test]
        fn promotes_every_formula_at_the_directory_its_native_entry_declares() {
            let recipe = Recipe::new("recipe-homebrew-promote", HOMEBREW_JOB)
                .with_destination("homebrew-tap")
                .with_distribution();
            recipe.run().expect_success();
            assert_eq!(
                recipe
                    .destination_files("homebrew-tap")
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![
                    "Formula/example-tool.rb".to_owned(),
                    "HomebrewFormula/example-tool.rb".to_owned(),
                ],
                "both declared taps receive their formula, and the cask is not one"
            );
        }

        // Destination readback decides whether a publication is present, so a
        // rerun that finds the tap already carrying this release has nothing to
        // do and must not fail for having nothing to do.
        #[test]
        fn a_homebrew_rerun_that_changes_nothing_commits_nothing() {
            let recipe = Recipe::new("recipe-homebrew-rerun", HOMEBREW_JOB)
                .with_destination("homebrew-tap")
                .with_distribution();
            recipe.run().expect_success();
            let first = recipe.destination_commits("homebrew-tap");
            recipe.run().expect_success();
            assert_eq!(
                recipe.destination_commits("homebrew-tap"),
                first,
                "an unchanged rerun succeeds without a second commit"
            );
        }

        #[test]
        fn a_homebrew_promotion_with_no_generated_formula_fails() {
            let recipe = Recipe::new("recipe-homebrew-absent", HOMEBREW_JOB)
                .with_destination("homebrew-tap");
            assert!(
                !recipe.run().succeeded(),
                "a build that produced no formula cannot be promoted"
            );
            assert!(recipe.destination_files("homebrew-tap").is_empty());
        }

        // The packager writes `aur/<package>.pkgbuild` and `.srcinfo`; the Arch
        // User Repository requires `PKGBUILD` and `.SRCINFO`. Both halves have
        // to be right, and this is the pair the previous recipe got wrong.
        #[test]
        fn installs_the_arch_sources_under_the_names_the_repository_requires() {
            let recipe = Recipe::new("recipe-aur-promote", AUR_JOB)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            recipe.run().expect_success();
            let files = recipe.destination_files(AUR_PACKAGE);
            assert_eq!(
                files.keys().cloned().collect::<Vec<_>>(),
                vec![".SRCINFO".to_owned(), "PKGBUILD".to_owned()],
                "the destination receives exactly the two files makepkg reads"
            );
            assert_eq!(files["PKGBUILD"], "pkgname=example-tool-bin\n");
            assert_eq!(files[".SRCINFO"], "pkgbase = example-tool-bin\n");
        }

        #[test]
        fn cargo_aur_route_promotes_the_aggregate_descriptors_without_rebuilding() {
            let workspace = cargo_system_package_workspace("recipe-cargo-aur-promote");
            converge(workspace.root(), WorkflowRole::Publish);
            let recipe = Recipe::from_workspace(
                workspace,
                "intentional_publish_component_utility_aur_primary",
            )
            .with_destination("sample-utility-bin");
            let bytes = recipe.temp.join("intentional_subject/bytes/aur");
            std::fs::create_dir_all(&bytes).expect("Cargo AUR descriptor directory");
            std::fs::write(
                bytes.join("sample-utility-bin.pkgbuild"),
                "pkgname=sample-utility-bin\nprovides=('sample-utility')\nconflicts=('sample-utility')\nsource_x86_64=('sealed archive')\n",
            )
            .expect("Cargo PKGBUILD");
            std::fs::write(
                bytes.join("sample-utility-bin.srcinfo"),
                "pkgbase = sample-utility-bin\n\tprovides = sample-utility\n\tconflicts = sample-utility\n\tpkgver = 1.2.3\n",
            )
            .expect("Cargo .SRCINFO");

            recipe.run().expect_success();
            let files = recipe.destination_files("sample-utility-bin");
            assert_eq!(
                files["PKGBUILD"],
                "pkgname=sample-utility-bin\nprovides=('sample-utility')\nconflicts=('sample-utility')\nsource_x86_64=('sealed archive')\n"
            );
            assert_eq!(
                files[".SRCINFO"],
                "pkgbase = sample-utility-bin\n\tprovides = sample-utility\n\tconflicts = sample-utility\n\tpkgver = 1.2.3\n"
            );
            assert!(
                !job_run_bodies(
                    &publish_jobs(recipe.workspace.root()),
                    "intentional_publish_component_utility_aur_primary"
                )
                .contains("cargo build"),
                "the publisher promotes the sealed descriptors"
            );
        }

        // A release unit with a second `aur` entry writes its files into the
        // same directory. Promoting them would publish another package's
        // sources under this package's name.
        #[test]
        fn leaves_a_sibling_arch_package_out_of_this_publication() {
            let recipe = Recipe::new("recipe-aur-sibling", AUR_JOB)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            recipe.run().expect_success();
            let files = recipe.destination_files(AUR_PACKAGE);
            assert!(
                !files
                    .values()
                    .any(|contents| contents.contains("example-other-bin")),
                "the sibling package's sources stayed out: {files:?}"
            );
        }

        // The Arch User Repository registers a package on its initial push, and
        // a clone of one it does not carry is an error rather than an empty
        // repository. A recipe that could only update would never publish a new
        // package at all.
        #[test]
        fn creates_an_arch_package_the_repository_does_not_yet_carry() {
            let recipe = Recipe::new("recipe-aur-first", AUR_JOB).with_distribution();
            assert!(!recipe.destination_exists(AUR_PACKAGE));
            recipe.run().expect_success();
            assert_eq!(
                recipe
                    .destination_files(AUR_PACKAGE)
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![".SRCINFO".to_owned(), "PKGBUILD".to_owned()]
            );
        }

        #[test]
        fn an_arch_rerun_that_changes_nothing_commits_nothing() {
            let recipe = Recipe::new("recipe-aur-rerun", AUR_JOB)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            recipe.run().expect_success();
            let first = recipe.destination_commits(AUR_PACKAGE);
            recipe.run().expect_success();
            assert_eq!(recipe.destination_commits(AUR_PACKAGE), first);
        }

        // The recipe scopes its SSH authority to one destination. Trusting the
        // host on first use would hand that authority to whatever answered.
        #[test]
        fn refuses_an_arch_host_whose_key_does_not_match_the_pin() {
            let recipe = Recipe::new("recipe-aur-host", AUR_JOB)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            let outcome = recipe.run_with_host_fingerprint(Some("SHA256:AnImpostorAnsweredHere"));
            assert!(
                !outcome.succeeded(),
                "an unpinned host key stops the promotion"
            );
            assert!(
                recipe.destination_files(AUR_PACKAGE).is_empty(),
                "nothing reached the destination"
            );
        }

        // An unnamed entry takes the project name and the same suffix, so it
        // names the same destination. Dropping it while reading would make the
        // sibling entry index 0 and promote this package's sources under the
        // sibling's name.
        #[test]
        fn resolves_an_unnamed_arch_entry_to_this_publications_package() {
            let recipe = Recipe::declaring("recipe-aur-unnamed", AUR_JOB, UNNAMED_ARCH_CONFIG)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            recipe.run().expect_success();
            let files = recipe.destination_files(AUR_PACKAGE);
            assert_eq!(files["PKGBUILD"], "pkgname=example-tool-bin\n");
            assert!(
                !recipe.destination_exists("example-other-bin"),
                "the sibling package was never reached"
            );
        }

        #[test]
        fn an_arch_promotion_with_no_generated_sources_fails() {
            let recipe = Recipe::new("recipe-aur-absent", AUR_JOB).with_destination(AUR_PACKAGE);
            assert!(!recipe.run().succeeded());
            assert!(recipe.destination_files(AUR_PACKAGE).is_empty());
        }
    }
