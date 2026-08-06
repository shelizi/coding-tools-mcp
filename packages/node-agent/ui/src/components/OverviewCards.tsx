import Alert from 'react-bootstrap/Alert';
import Badge from 'react-bootstrap/Badge';
import Card from 'react-bootstrap/Card';
import Col from 'react-bootstrap/Col';
import Row from 'react-bootstrap/Row';
import Stack from 'react-bootstrap/Stack';
import { formatBytes, formatDuration, percent } from '../format';
import type { DashboardPayload, ManagementStatus, WorkspaceConfigSnapshot } from '../types';
import { MetricCard } from './MetricCard';

interface OverviewCardsProps {
  status: ManagementStatus;
  dashboard: DashboardPayload;
  config: WorkspaceConfigSnapshot;
}

function healthTone(state: DashboardPayload['health']['state']): 'success' | 'warning' | 'danger' {
  if (state === 'healthy') return 'success';
  if (state === 'busy') return 'warning';
  return 'danger';
}

function healthLabel(state: DashboardPayload['health']['state']): string {
  if (state === 'healthy') return '健康';
  if (state === 'busy') return '忙碌';
  return '降級';
}

export function OverviewCards({ status, dashboard, config }: OverviewCardsProps) {
  const blocking = dashboard.admission.blocking;
  const processLane = dashboard.admission.process;
  const tunnelWorkers = dashboard.tunnel.workers ?? 0;
  const connectedWorkers = dashboard.tunnel.connectedWorkers ?? 0;
  const busyWorkers = dashboard.tunnel.busyWorkers ?? 0;

  return (
    <Stack gap={4}>
      {config.restartRequired ? (
        <Alert variant="warning" className="mb-0">
          設定已變更，重新啟動 Agent 後才會套用。
        </Alert>
      ) : null}
      <Card className="panel-card">
        <Card.Header className="panel-heading">執行狀態</Card.Header>
        <Card.Body>
          <Row className="g-3">
            <Col xs={6} lg={3}><MetricCard label="MCP 工具" value={status.tools} detail={status.toolProfile} /></Col>
            <Col xs={6} lg={3}><MetricCard label="命令 Sessions" value={status.sessions.total} detail={`${status.sessions.running} running`} /></Col>
            <Col xs={6} lg={3}><MetricCard label="工作區" value={status.workspaces.length} /></Col>
            <Col xs={6} lg={3}><MetricCard label="Built-in WSS" value={status.tunnel?.state ?? 'disabled'} /></Col>
          </Row>
          <div className="path-list mt-3">
            <div><span>設定檔</span><code>{config.configPath}</code></div>
            <div><span>Schema</span><code>v{config.schemaVersion}</code></div>
            <div><span>加密秘密</span><code>{config.secretStorePath}</code></div>
          </div>
          <div className="text-secondary small mt-3">
            {config.migrationApplied
              ? `已將舊 schema v${config.migratedFromSchema ?? 0} 的明文秘密遷移到 AES-256-GCM 加密儲存。`
              : '設定已使用最新 schema。'}
          </div>
        </Card.Body>
      </Card>

      <Card className="panel-card">
        <Card.Header className="panel-heading d-flex justify-content-between align-items-center">
          <span>健康度與負載</span>
          <Badge bg={healthTone(dashboard.health.state)}>{healthLabel(dashboard.health.state)}</Badge>
        </Card.Header>
        <Card.Body>
          <Row className="g-3">
            <Col xs={6} lg={3}>
              <MetricCard label="運行時間" value={formatDuration(dashboard.health.uptimeMs)} tone={healthTone(dashboard.health.state)} />
            </Col>
            <Col xs={6} lg={3}>
              <MetricCard label="RSS 記憶體" value={formatBytes(dashboard.runtime.memory.rssBytes)} detail={`Heap ${formatBytes(dashboard.runtime.memory.heapUsedBytes)}`} />
            </Col>
            <Col xs={6} lg={3}>
              <MetricCard label="最近錯誤" value={`${dashboard.health.recentErrors}/${dashboard.health.recentCalls}`} detail={percent(dashboard.health.recentErrorRate)} tone={dashboard.health.recentErrors ? 'danger' : 'success'} />
            </Col>
            <Col xs={6} lg={3}>
              <MetricCard label="待授權" value={dashboard.permissions.pending} tone={dashboard.permissions.pending ? 'warning' : 'success'} />
            </Col>
          </Row>
          <Row className="g-3 mt-0">
            <Col md={6} xl={3}><MetricCard label="Blocking Lane" value={`${blocking.active}/${blocking.limit}`} detail={`${blocking.queued} queued`} tone={blocking.queued ? 'warning' : 'default'} /></Col>
            <Col md={6} xl={3}><MetricCard label="Process Lane" value={`${processLane.active}/${processLane.limit}`} detail={`${processLane.queued} queued`} tone={processLane.queued ? 'warning' : 'default'} /></Col>
            <Col md={6} xl={3}><MetricCard label="Tasks" value={dashboard.tasks.total} detail={Object.entries(dashboard.tasks.byStatus).map(([key, value]) => `${key}:${value}`).join(' · ') || 'none'} /></Col>
            <Col md={6} xl={3}><MetricCard label="WSS Workers" value={`${connectedWorkers}/${tunnelWorkers}`} detail={`${busyWorkers} busy`} /></Col>
          </Row>
          <div className="runtime-meta mt-3">
            <span>Agent {dashboard.runtime.version}</span>
            <span>{dashboard.runtime.nodeVersion}</span>
            <span>{dashboard.runtime.platform}/{dashboard.runtime.arch}</span>
            <span>PID {dashboard.runtime.pid}</span>
            <span>Profile {status.toolProfile}</span>
            <span>Revision {status.toolsetRevision}</span>
          </div>
        </Card.Body>
      </Card>
    </Stack>
  );
}
