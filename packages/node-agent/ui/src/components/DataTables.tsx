import Badge from 'react-bootstrap/Badge';
import Card from 'react-bootstrap/Card';
import Stack from 'react-bootstrap/Stack';
import Table from 'react-bootstrap/Table';
import { formatBytes, formatDuration, formatTime } from '../format';
import type { DashboardPayload, SessionSummary } from '../types';

interface DataTablesProps {
  dashboard: DashboardPayload;
}

function sessionVariant(session: SessionSummary): string {
  if (session.status === 'exited' && session.exitCode === 0) return 'success';
  if (session.status === 'running' || session.status === 'verifying') return 'warning';
  if (session.status === 'killed' || session.status === 'timed_out' || (session.exitCode ?? 0) !== 0) return 'danger';
  return 'secondary';
}

function EmptyRow({ columns }: { columns: number }) {
  return <tr><td colSpan={columns} className="text-center text-secondary py-4">尚無資料</td></tr>;
}

export function DataTables({ dashboard }: DataTablesProps) {
  return (
    <Stack gap={4}>
      <Card className="panel-card">
        <Card.Header className="panel-heading">命令 Sessions</Card.Header>
        <Card.Body className="p-0">
          <Table responsive hover className="align-middle mb-0 dashboard-table">
            <thead><tr><th>狀態</th><th>工作區 / CWD</th><th>開始</th><th>耗時</th><th>Exit</th><th>輸出</th></tr></thead>
            <tbody>
              {dashboard.sessions.items.length ? dashboard.sessions.items.map(session => (
                <tr key={session.id}>
                  <td><Badge bg={sessionVariant(session)}>{session.status}</Badge></td>
                  <td><strong>{session.workspaceName ?? '未對應'}</strong><div className="small text-secondary text-break">{session.cwd}</div></td>
                  <td>{formatTime(session.startedAt)}</td>
                  <td>{formatDuration(session.durationMs)}</td>
                  <td>{session.exitCode ?? '-'}</td>
                  <td>{formatBytes(session.stdoutBytes + session.stderrBytes)}</td>
                </tr>
              )) : <EmptyRow columns={6} />}
            </tbody>
          </Table>
        </Card.Body>
        <Card.Footer className="small text-secondary">
          僅顯示執行狀態與計量；command、環境變數、stdin、stdout/stderr 與 post-check 內容不會傳到 Dashboard。
        </Card.Footer>
      </Card>

      <Card className="panel-card">
        <Card.Header className="panel-heading">工具統計</Card.Header>
        <Card.Body className="p-0">
          <Table responsive hover className="align-middle mb-0 dashboard-table">
            <thead><tr><th>Tool</th><th>呼叫</th><th>錯誤</th><th>平均</th><th>P95</th><th>Queue</th><th>Response</th></tr></thead>
            <tbody>
              {dashboard.usage.aggregate.length ? dashboard.usage.aggregate.map(item => (
                <tr key={item.tool}>
                  <td><code>{item.tool}</code></td>
                  <td>{item.calls}</td>
                  <td className={item.errors ? 'text-danger fw-semibold' : ''}>{item.errors}</td>
                  <td>{formatDuration(item.averageDurationMs)}</td>
                  <td>{formatDuration(item.p95DurationMs)}</td>
                  <td>{formatDuration(item.averageQueueWaitMs)}</td>
                  <td>{formatBytes(item.responseBytes)}</td>
                </tr>
              )) : <EmptyRow columns={7} />}
            </tbody>
          </Table>
        </Card.Body>
      </Card>

      <Card className="panel-card">
        <Card.Header className="panel-heading">最近活動</Card.Header>
        <Card.Body className="p-0">
          <Table responsive hover className="align-middle mb-0 dashboard-table">
            <thead><tr><th>時間</th><th>Tool</th><th>工作區</th><th>結果</th><th>耗時</th></tr></thead>
            <tbody>
              {dashboard.activity.length ? dashboard.activity.map((item, index) => (
                <tr key={`${item.startedAt}-${item.tool}-${index}`}>
                  <td>{formatTime(item.startedAt)}</td>
                  <td><code>{item.tool}</code></td>
                  <td>{item.workspaceId ?? 'hub'}</td>
                  <td><Badge bg={item.ok ? 'success' : 'danger'}>{item.ok ? '成功' : '失敗'}</Badge></td>
                  <td>{formatDuration(item.durationMs)}</td>
                </tr>
              )) : <EmptyRow columns={5} />}
            </tbody>
          </Table>
        </Card.Body>
      </Card>
    </Stack>
  );
}
