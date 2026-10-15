#!/usr/bin/env bash
# Rebuild the Node Agent image, then replace the running Compose container.
# The previous image is kept as rollback-pre-<sha>. Health failure restores it.
#
# Tags:
#   coding-tools-node-agent:local                 production pointer
#   coding-tools-node-agent:<shortsha>            convenience build tag
#   coding-tools-node-agent:sha-<fullsha>         immutable build tag
#   coding-tools-node-agent:rollback-pre-<sha>    snapshot taken before switching to <sha>
#
# Usage:
#   scripts/switch-node-agent-docker.sh              # rebuild + safe switch
#   scripts/switch-node-agent-docker.sh --build-only
#   scripts/switch-node-agent-docker.sh --no-build --tag <tag>
#   scripts/switch-node-agent-docker.sh --rollback
#   scripts/switch-node-agent-docker.sh --status
#   scripts/switch-node-agent-docker.sh --dry-run
set -Eeuo pipefail

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_dir"

service="${CTMCP_SWITCH_SERVICE:-node-agent}"
compose_file="${CTMCP_SWITCH_COMPOSE_FILE:-docker-compose.yml}"
project="${CTMCP_SWITCH_PROJECT:-coding-tools-mcp}"
stable_image="${CTMCP_NODE_AGENT_IMAGE:-coding-tools-node-agent:local}"
health_path="${CTMCP_SWITCH_HEALTH_PATH:-/health}"
ui_path="${CTMCP_SWITCH_UI_PATH:-/ui/}"
health_timeout="${CTMCP_SWITCH_HEALTH_TIMEOUT:-90}"
stop_timeout="${CTMCP_SWITCH_STOP_TIMEOUT:-20}"
state_dir="${repo_dir}/.local"
state_file="${CTMCP_SWITCH_STATE_FILE:-${state_dir}/node-agent-docker-switch.env}"

mode="switch"
do_build=1
dry_run=0
skip_ui=0
explicit_tag=""

usage() {
  cat <<'EOF'
Rebuild coding-tools-node-agent, then safely replace the Compose container.

  scripts/switch-node-agent-docker.sh [options]

Options:
  --build-only          Build and tag the new image; do not touch the container
  --no-build            Skip docker build (use --tag or the current git sha tag)
  --tag TAG             Image to switch to (name:tag or tag)
  --rollback            Restore the last rollback-pre-* snapshot
  --status              Print current container, image tags, and last switch
  --dry-run             Print the plan without building or replacing
  --skip-ui             Do not wait for /ui/ after /health
  -h, --help            Show this help

Environment:
  CTMCP_NODE_AGENT_IMAGE          Compose image name (default coding-tools-node-agent:local)
  CTMCP_SWITCH_PROJECT            Compose project name (default coding-tools-mcp)
  CTMCP_SWITCH_HEALTH_TIMEOUT     Seconds to wait for /health (default 90)
  CTMCP_SWITCH_STATE_FILE         Switch/rollback state path (default .local/node-agent-docker-switch.env)
  CTMCP_OAUTH_PASSWORD            Required when docker-compose.yml interpolates it
EOF
}

fail() {
  printf 'switch-node-agent-docker: %s\n' "$*" >&2
  exit 1
}

log() {
  printf 'switch-node-agent-docker: %s\n' "$*" >&2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --build-only) mode="build-only"; shift ;;
    --no-build) do_build=0; shift ;;
    --tag)
      [[ $# -ge 2 ]] || fail '--tag requires an image tag'
      explicit_tag="$2"
      shift 2
      ;;
    --rollback) mode="rollback"; do_build=0; shift ;;
    --status) mode="status"; shift ;;
    --dry-run) dry_run=1; shift ;;
    --skip-ui) skip_ui=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

command -v docker >/dev/null || fail 'docker is required'
docker compose version >/dev/null || fail 'docker compose is required'

compose() {
  docker compose -p "$project" -f "$compose_file" "$@"
}

image_repo="${stable_image%%:*}"
[[ "$image_repo" == "$stable_image" ]] && image_repo="coding-tools-node-agent"
[[ -n "$image_repo" ]] || fail "could not parse image repository from ${stable_image}"

short_sha="$(git -C "$repo_dir" rev-parse --short HEAD 2>/dev/null || true)"
full_sha="$(git -C "$repo_dir" rev-parse HEAD 2>/dev/null || true)"
if [[ -z "$short_sha" ]]; then
  short_sha="$(date -u +%Y%m%d%H%M%S)"
  full_sha="unknown"
fi
source_clean="false"
if git -C "$repo_dir" diff --quiet --ignore-submodules HEAD 2>/dev/null \
  && git -C "$repo_dir" diff --quiet --ignore-submodules --cached 2>/dev/null \
  && [[ -z "$(git -C "$repo_dir" ls-files --others --exclude-standard 2>/dev/null)" ]]; then
  source_clean="true"
fi

normalize_tag() {
  local tag="$1"
  if [[ "$tag" == *:* ]]; then
    printf '%s\n' "$tag"
  else
    printf '%s:%s\n' "$image_repo" "$tag"
  fi
}

container_id() {
  compose ps -q "$service" 2>/dev/null || true
}

container_name_of() {
  local id="$1"
  [[ -n "$id" ]] || return 0
  docker inspect "$id" --format '{{.Name}}' | sed 's#^/##'
}

image_id_of() {
  docker image inspect "$1" --format '{{.Id}}' 2>/dev/null || true
}

running_image_id() {
  local id
  id="$(container_id)"
  [[ -n "$id" ]] || return 0
  docker inspect "$id" --format '{{.Image}}'
}

host_port() {
  local id="$1"
  [[ -n "$id" ]] || { printf '%s\n' "${CTMCP_PORT:-3789}"; return 0; }
  local port
  port="$(docker inspect "$id" --format '{{(index (index .NetworkSettings.Ports "3789/tcp") 0).HostPort}}' 2>/dev/null || true)"
  if [[ -z "$port" || "$port" == '<no value>' ]]; then
    printf '%s\n' "${CTMCP_PORT:-3789}"
  else
    printf '%s\n' "$port"
  fi
}

health_ok() {
  local id="$1"
  local path="$2"
  if [[ -n "$id" ]] && docker exec "$id" node -e "fetch('http://127.0.0.1:3789${path}').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))" >/dev/null 2>&1; then
    return 0
  fi
  local port
  port="$(host_port "$id")"
  curl -fsS -o /dev/null --max-time 3 "http://127.0.0.1:${port}${path}" 2>/dev/null
}

wait_ready() {
  local id="$1"
  local attempts="$health_timeout"
  local attempt
  for attempt in $(seq 1 "$attempts"); do
    if health_ok "$id" "$health_path"; then
      if [[ "$skip_ui" == "1" ]] || health_ok "$id" "$ui_path"; then
        return 0
      fi
    fi
    sleep 1
  done
  return 1
}

in_target_container() {
  [[ -f /.dockerenv ]] || return 1
  local target
  target="$(container_id)"
  [[ -n "$target" ]] || return 1
  local hostname full_id
  hostname="$(hostname)"
  full_id="$(docker inspect "$target" --format '{{.Id}}')"
  [[ "$full_id" == "$hostname"* || "${full_id:0:12}" == "$hostname" ]]
}

remove_created_replacements() {
  local id state names
  while IFS= read -r id; do
    [[ -n "$id" ]] || continue
    state="$(docker inspect "$id" --format '{{.State.Status}}' 2>/dev/null || true)"
    names="$(docker inspect "$id" --format '{{.Name}}' 2>/dev/null || true)"
    if [[ "$state" == "created" ]]; then
      log "removing leftover Created container ${names#/} ($id)"
      docker rm -f "$id" >/dev/null
    fi
  done < <(docker ps -aq --filter "label=com.docker.compose.project=${project}" --filter "label=com.docker.compose.service=${service}")
}

write_state() {
  mkdir -p "$state_dir"
  cat > "$state_file" <<EOF
SWITCHED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
SHORT_SHA=${short_sha}
FULL_SHA=${full_sha}
TO_TAG=${to_tag}
ROLLBACK_TAG=${rollback_tag}
FROM_IMAGE_ID=${from_image_id}
STABLE_IMAGE=${stable_image}
EOF
}

load_state() {
  [[ -f "$state_file" ]] || fail "no previous switch state at ${state_file}"
  # shellcheck disable=SC1090
  source "$state_file"
  [[ -n "${ROLLBACK_TAG:-}" ]] || fail "state file is missing ROLLBACK_TAG"
}

print_status() {
  local id name image status port
  id="$(container_id)"
  printf 'project=%s service=%s compose=%s\n' "$project" "$service" "$compose_file"
  printf 'stable_image=%s\n' "$stable_image"
  if [[ -n "$id" ]]; then
    name="$(container_name_of "$id")"
    image="$(docker inspect "$id" --format '{{.Config.Image}} {{.Image}}')"
    status="$(docker inspect "$id" --format '{{.State.Status}} health={{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}')"
    port="$(host_port "$id")"
    printf 'container=%s id=%s\n' "$name" "$id"
    printf 'image=%s\n' "$image"
    printf 'status=%s port=%s\n' "$status" "$port"
    if health_ok "$id" "$health_path"; then
      printf 'health=ok url=http://127.0.0.1:%s%s\n' "$port" "$health_path"
    else
      printf 'health=down url=http://127.0.0.1:%s%s\n' "$port" "$health_path"
    fi
  else
    printf 'container=none\n'
  fi
  if docker image inspect "$stable_image" >/dev/null 2>&1; then
    printf 'local_id=%s tags=%s\n' "$(image_id_of "$stable_image")" "$(docker image inspect "$stable_image" --format '{{json .RepoTags}}')"
  else
    printf 'local_id=missing\n'
  fi
  if [[ -f "$state_file" ]]; then
    printf 'last_switch_state=%s\n' "$state_file"
    sed 's/^/  /' "$state_file"
  else
    printf 'last_switch_state=none\n'
  fi
}

resolve_to_tag() {
  if [[ -n "$explicit_tag" ]]; then
    normalize_tag "$explicit_tag"
  elif [[ "$full_sha" != "unknown" ]]; then
    printf '%s:sha-%s\n' "$image_repo" "$full_sha"
  else
    printf '%s:%s\n' "$image_repo" "$short_sha"
  fi
}

build_image() {
  local tag="$1"
  log "building ${tag} (BUILD_GIT_SHA=${full_sha} SOURCE_CLEAN=${source_clean})"
  docker build \
    --file packages/node-agent/Dockerfile \
    --build-arg "BUILD_GIT_SHA=${full_sha}" \
    --build-arg "BUILD_SOURCE_CLEAN=${source_clean}" \
    --tag "$tag" \
    --tag "${image_repo}:${short_sha}" \
    .
}

remove_service_container() {
  local id="$1"
  local name
  [[ -n "$id" ]] || return 0
  name="$(container_name_of "$id")"
  docker update --restart=no "$id" >/dev/null 2>&1 || true
  log "stopping ${name}"
  docker stop -t "$stop_timeout" "$id" >/dev/null
  docker rm -f "$id" >/dev/null
  log "removed ${name}"
}

start_with_image() {
  local image="$1"
  log "starting ${service} from ${image}"
  CTMCP_NODE_AGENT_IMAGE="$image" compose up -d --no-build --remove-orphans "$service" >&2
  local id
  id="$(container_id)"
  [[ -n "$id" ]] || fail "compose did not create ${service}"
  printf '%s\n' "$id"
}

restore_previous() {
  local reason="$1"
  log "ERROR: ${reason}; rolling back"
  local new_id
  new_id="$(container_id)"
  remove_service_container "$new_id"
  remove_created_replacements
  local restore_image="$stable_image"
  if docker image inspect "$rollback_tag" >/dev/null 2>&1; then
    restore_image="$rollback_tag"
  elif [[ -n "${from_image_id:-}" ]]; then
    docker tag "$from_image_id" "$stable_image"
    restore_image="$stable_image"
  fi
  local id
  id="$(start_with_image "$restore_image")"
  if [[ "$restore_image" != "$stable_image" ]]; then
    docker tag "$restore_image" "$stable_image"
  fi
  if wait_ready "$id"; then
    log "rollback container is healthy"
  else
    fail "rollback started but health did not recover"
  fi
}

safe_replace() {
  local new_tag="$1"
  local snapshot="$2"
  docker image inspect "$new_tag" >/dev/null 2>&1 || fail "image not found: ${new_tag}"

  local old_id
  old_id="$(container_id)"
  from_image_id="$(running_image_id)"
  if [[ -z "$from_image_id" ]]; then
    from_image_id="$(image_id_of "$stable_image")"
  fi
  to_tag="$new_tag"
  rollback_tag="${image_repo}:rollback-pre-${short_sha}"

  if [[ "$snapshot" == "1" && -n "$from_image_id" ]]; then
    docker tag "$from_image_id" "$rollback_tag"
    log "tagged rollback snapshot ${rollback_tag}"
  elif [[ "$snapshot" != "1" ]]; then
    rollback_tag="${ROLLBACK_TAG:-$rollback_tag}"
    log "rollback snapshot left unchanged (${rollback_tag})"
  else
    log "no previous image to snapshot"
  fi

  local new_id
  new_id="$(image_id_of "$new_tag")"
  if [[ -n "$from_image_id" && "$new_id" == "$from_image_id" ]]; then
    docker tag "$new_tag" "$stable_image"
    write_state
    log "target image is already running; ${stable_image} retagged"
    return 0
  fi

  if in_target_container; then
    fail "refusing to replace the running container from inside itself (this caused the Created/Exited name race). Build is done; on the host run: $0 --no-build --tag ${new_tag}"
  fi

  remove_created_replacements
  remove_service_container "$old_id"

  local started=""
  if ! started="$(start_with_image "$new_tag")"; then
    restore_previous 'compose up failed'
    fail 'switch failed'
  fi
  if ! wait_ready "$started"; then
    printf '%s\n' '--- new container logs ---' >&2
    docker logs --tail 80 "$started" >&2 || true
    restore_previous "new container failed health within ${health_timeout}s"
    fail 'switch failed'
  fi

  docker tag "$new_tag" "$stable_image"
  write_state
  log "healthy: $(container_name_of "$started") image=${new_tag}"
}

print_plan() {
  printf 'mode=%s build=%s dry_run=%s\n' "$mode" "$do_build" "$dry_run"
  printf 'sha=%s full=%s source_clean=%s\n' "$short_sha" "$full_sha" "$source_clean"
  printf 'to_tag=%s\n' "$(resolve_to_tag)"
  printf 'rollback_tag=%s:%s\n' "$image_repo" "rollback-pre-${short_sha}"
  printf 'stable_image=%s\n' "$stable_image"
  print_status
  if in_target_container; then
    printf 'in_target_container=yes (switch will refuse; build-only is allowed)\n'
  else
    printf 'in_target_container=no\n'
  fi
}

case "$mode" in
  status)
    print_status
    exit 0
    ;;
  rollback)
    load_state
    short_sha="${SHORT_SHA:-$short_sha}"
    full_sha="${FULL_SHA:-$full_sha}"
    explicit_tag="${ROLLBACK_TAG}"
    do_build=0
    if [[ "$dry_run" == "1" ]]; then
      print_plan
      exit 0
    fi
    docker image inspect "$ROLLBACK_TAG" >/dev/null 2>&1 || fail "rollback image missing: ${ROLLBACK_TAG}"
    safe_replace "$ROLLBACK_TAG" 0
    log "rolled back to ${ROLLBACK_TAG}"
    print_status
    ;;
  build-only)
    to_tag="$(resolve_to_tag)"
    if [[ "$dry_run" == "1" ]]; then
      print_plan
      exit 0
    fi
    build_image "$to_tag"
    log "built ${to_tag}; production container was not replaced"
    docker image inspect "$to_tag" --format 'id={{.Id}} created={{.Created}}'
    ;;
  switch)
    to_tag="$(resolve_to_tag)"
    rollback_tag="${image_repo}:rollback-pre-${short_sha}"
    if [[ "$dry_run" == "1" ]]; then
      print_plan
      exit 0
    fi
    if [[ "$do_build" == "1" ]]; then
      build_image "$to_tag"
    else
      docker image inspect "$to_tag" >/dev/null 2>&1 || fail "image not found: ${to_tag} (build first or pass --tag)"
    fi
    safe_replace "$to_tag" 1
    print_status
    ;;
  *)
    fail "internal error: unknown mode ${mode}"
    ;;
esac
