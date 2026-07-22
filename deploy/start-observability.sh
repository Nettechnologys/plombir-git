#!/usr/bin/env bash
# IronForge Observability Quick Start
# Phase 22-C

set -e

cd "$(dirname "$0")"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  IronForge Observability Stack — Phase 22-C"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Check Docker
if ! command -v docker &> /dev/null; then
    echo "❌ Docker not found. Please install Docker first."
    exit 1
fi

# Check if IronForge is running
echo "🔍 Checking if IronForge is running on :7878..."
if curl -s -o /dev/null -w "%{http_code}" http://localhost:7878/health 2>/dev/null | grep -q "200\|404\|401"; then
    echo "✅ IronForge detected"
else
    echo "⚠️  IronForge not detected on :7878 (will still start the stack)"
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
echo "  IronForge:      http://localhost:7878/metrics"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "📈 Try these PromQL queries in Prometheus:"
echo "  • sum(rate(http_requests_total[5m]))  (QPS)"
echo "  • histogram_quantile(0.95, sum by (le, route) (rate(http_request_duration_seconds_bucket[5m])))"
echo "  • ironforge_repositories  (total repos)"
echo "  • up{job=\"ironforge\"}  (health)"
echo ""
echo "🔥 To stop the stack:"
echo "  docker compose -f docker-compose.observability.yml down"
