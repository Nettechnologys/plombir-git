#!/usr/bin/env bash
# Plombir Git Observability Quick Start
# Phase 22-C

set -e

cd "$(dirname "$0")"

MAIN_COMPOSE="docker-compose.yml"

compose_http_ports() {
    local compose="$1"
    local normalized_compose
    local -a marked_mappings normalized_mappings

    if [ ! -r "${compose}" ]; then
        echo "❌ Cannot read ${compose}; cannot determine the Plombir Git HTTP port." >&2
        return 1
    fi

    mapfile -t marked_mappings < <(
        awk '
            /^[[:space:]]*-[[:space:]]*"?[0-9]+:[0-9]+"?[[:space:]]*#[[:space:]]*HTTP[[:space:]]*$/ { print }
        ' "${compose}"
    )

    if [ "${#marked_mappings[@]}" -ne 1 ] ||
        [[ ! "${marked_mappings[0]}" =~ ^[[:space:]]*-[[:space:]]*\"?([0-9]+):([0-9]+)\"?[[:space:]]*#[[:space:]]*HTTP[[:space:]]*$ ]]; then
        echo "❌ ${compose}: expected exactly one numeric HOST:CONTAINER Plombir Git port mapping marked # HTTP." >&2
        return 1
    fi

    local marked_host_port="${BASH_REMATCH[1]}"
    local marked_container_port="${BASH_REMATCH[2]}"

    if ! normalized_compose="$(
        docker compose -f "${compose}" config --no-interpolate
    )"; then
        echo "❌ ${compose}: docker compose config failed; cannot determine the Plombir Git HTTP port." >&2
        return 1
    fi

    # Docker Compose owns YAML semantics here. Its normalized output expands
    # every port into long syntax, so this scanner only has to walk the stable
    # services.*.ports[*].{published,target} paths. The service name stays in
    # the result: a same-valued sidecar mapping must not impersonate Plombir Git.
    mapfile -t normalized_mappings < <(
        awk '
            function flush_port() {
                if (published ~ /^[0-9]+$/ && target ~ /^[0-9]+$/) {
                    print service " " published " " target
                }
                published = ""
                target = ""
            }

            function close_ports() {
                if (inside_ports) flush_port()
                inside_ports = 0
            }

            /^services:[[:space:]]*$/ { inside_services = 1; next }
            inside_services && /^[^[:space:]]/ { close_ports(); exit }
            inside_services && /^  [^[:space:]][^:]*:[[:space:]]*$/ {
                close_ports()
                service = $0
                sub(/^  /, "", service)
                sub(/:[[:space:]]*$/, "", service)
                inside_service = 1
                next
            }
            inside_service && /^    ports:[[:space:]]*$/ { inside_ports = 1; next }
            inside_ports && /^    [^[:space:]]/ { close_ports() }
            inside_ports && /^      -[[:space:]]/ { flush_port(); next }
            inside_ports && /^        published:[[:space:]]*"?[0-9]+"?[[:space:]]*$/ {
                published = $0
                sub(/^        published:[[:space:]]*"?/, "", published)
                sub(/"?[[:space:]]*$/, "", published)
                next
            }
            inside_ports && /^        target:[[:space:]]*"?[0-9]+"?[[:space:]]*$/ {
                target = $0
                sub(/^        target:[[:space:]]*"?/, "", target)
                sub(/"?[[:space:]]*$/, "", target)
                next
            }

            END { close_ports() }
        ' <<<"${normalized_compose}"
    )

    local matching_entries=0
    local plombir_git_entries=0
    local mapping
    local service published target
    for mapping in "${normalized_mappings[@]}"; do
        read -r service published target <<<"${mapping}"
        if [ "${published} ${target}" = "${marked_host_port} ${marked_container_port}" ]; then
            ((matching_entries += 1))
            if [ "${service}" = "plombir-git" ]; then
                ((plombir_git_entries += 1))
            fi
        fi
    done

    if [ "${matching_entries}" -ne 1 ] || [ "${plombir_git_entries}" -ne 1 ]; then
        echo "❌ ${compose}: mapping marked # HTTP does not identify one unique services.plombir-git.ports entry after docker compose config." >&2
        return 1
    fi

    printf '%s %s\n' "${marked_host_port}" "${marked_container_port}"
}

if ! command -v docker &> /dev/null; then
    echo "❌ Docker not found. Please install Docker first."
    exit 1
fi

if ! PLOMBIR_GIT_HTTP_PORTS="$(compose_http_ports "${MAIN_COMPOSE}")"; then
    exit 1
fi
read -r PLOMBIR_GIT_HOST_PORT PLOMBIR_GIT_CONTAINER_PORT <<<"${PLOMBIR_GIT_HTTP_PORTS}"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Plombir Git Observability Stack — Phase 22-C"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Check if Plombir Git is running
echo "🔍 Checking if Plombir Git is running on :${PLOMBIR_GIT_HOST_PORT}..."
if curl -s -o /dev/null -w "%{http_code}" "http://localhost:${PLOMBIR_GIT_HOST_PORT}/health" 2>/dev/null | grep -q "200\|404\|401"; then
    echo "✅ Plombir Git detected"
else
    echo "⚠️  Plombir Git not detected on :${PLOMBIR_GIT_HOST_PORT} (will still start the stack)"
fi

# Start the stack
echo ""
echo "🚀 Starting Prometheus + Grafana + Alertmanager..."
docker compose -f docker-compose.observability.yml up -d

echo ""
echo "⏳ Waiting for services to be healthy (max 60s)..."
for i in {1..12}; do
    sleep 5
    PROM_OK=$(curl -s -o /dev/null -w "%{http_code}" http://localhost:9090/-/ready 2>/dev/null || echo "000")
    GRAFANA_OK=$(curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/api/health 2>/dev/null || echo "000")
    AM_OK=$(curl -s -o /dev/null -w "%{http_code}" http://localhost:9093/-/ready 2>/dev/null || echo "000")

    echo "  [$((i*5))s] Prometheus=$PROM_OK Grafana=$GRAFANA_OK Alertmanager=$AM_OK"

    if [ "$PROM_OK" = "200" ] && [ "$GRAFANA_OK" = "200" ] && [ "$AM_OK" = "200" ]; then
        echo ""
        echo "✅ All services healthy!"
        break
    fi
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  📊 Service Endpoints"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Prometheus:     http://localhost:9090"
echo "  Grafana:        http://localhost:3000  (admin/admin)"
echo "  Alertmanager:   http://localhost:9093"
echo "  Node Exporter:  http://localhost:9100/metrics"
echo ""
echo "  Plombir Git:      http://localhost:${PLOMBIR_GIT_HOST_PORT}/metrics"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "📈 Try these PromQL queries in Prometheus:"
echo "  • sum(rate(http_requests_total[5m]))  (QPS)"
echo "  • histogram_quantile(0.95, sum by (le, route) (rate(http_request_duration_seconds_bucket[5m])))"
echo "  • plombir_git_repositories  (total repos)"
echo "  • up{job=\"plombir-git\"}  (health)"
echo ""
echo "🔥 To stop the stack:"
echo "  docker compose -f docker-compose.observability.yml down"
