# Portable release artifact publication

Portable builds, dependency trees, Cargo targets, temporary staging and tests stay on D:. The checked-out source tree is not used to compile.

After a successful live runtime handoff and health check, the switch scripts invoke `scripts/promote-portable-release.ps1` to publish a verified, immutable ZIP and canonical extracted directory under the main Git checkout:

- Rust: `dist-portable/ctmcp-win64` and `dist-portable/ctmcp-<version>-win64.zip`
- Node: `dist-node-portable/ctnode-win64` and `dist-node-portable/ctnode-<version>-p<portable>-win64.zip`

The publisher verifies SHA-256 and critical Node package checksums, stages the archive and extracted files, moves previous files into `previous/<unique-id>`, and restores them on publication errors. Existing user configuration and data directories are excluded. Previous backups are retained rather than automatically removed.

Node switch detects the accompanying ZIP beside `-PackageRoot` and publishes after confirming all saved endpoints report the expected version and Git commit. Use `-ReleaseArchive` and `-PublishRoot` to override the defaults. If no ZIP is available, the switch still works without publication; the handoff log records the runtime result.

Rust switch verifies the replacement executable and all enabled endpoints first; then publishes `-PackageZip` to the main checkout (or `-PublishRoot`). A publication problem must be reported separately: a healthy running runtime is not misrepresented as a failed restart.

The presence of files under `dist*` alone is not evidence that a new runtime is serving requests. Always verify the live `server_info.runtime_revision.build_git_sha` and endpoint health.
