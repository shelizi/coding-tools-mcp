import { useEffect, useState } from 'react';
import Alert from 'react-bootstrap/Alert';
import Badge from 'react-bootstrap/Badge';
import Button from 'react-bootstrap/Button';
import Col from 'react-bootstrap/Col';
import Form from 'react-bootstrap/Form';
import Row from 'react-bootstrap/Row';
import Spinner from 'react-bootstrap/Spinner';
import Table from 'react-bootstrap/Table';
import { fetchWorkspaceTelemetry } from '../api';
import { formatDuration, formatTime } from '../format';
import { useI18n } from '../i18n';
import type { TelemetryFilters, TelemetryPayload, TelemetryRecord, TelemetryScope, TelemetrySort } from '../types';

const DEFAULT_FILTERS: TelemetryFilters = {
  scope: 'current_runtime',
  errorsOnly: false,
  limit: 100,
  minDurationMs: 0,
  sortBy: 'calls'
};

function errorText(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function recordTitle(record: TelemetryRecord): string {
  if (record.event === 'async_session_finalized') return record.command_kind ? `async · ${record.command_kind}` : 'async session';
  return record.tool ?? 'unknown';
}

function recordOutcome(record: TelemetryRecord): string {
  return String(record.outcome_class ?? record.outcome ?? (record.is_error ? 'error' : 'success'));
}

export function TelemetryView({ workspaceId }: { workspaceId: string }) {
  const { t } = useI18n();
  const [filters, setFilters] = useState<TelemetryFilters>(DEFAULT_FILTERS);
  const [data, setData] = useState<TelemetryPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError('');
    void fetchWorkspaceTelemetry(workspaceId, filters, controller.signal)
      .then(setData)
      .catch(reason => {
        if (!controller.signal.aborted) setError(errorText(reason));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [workspaceId, filters]);

  const refresh = () => setFilters(current => ({ ...current }));
  const aggregate = data?.aggregate;

  return (
    <div className="tx-observability-stack">
      <article className="tx-card">
        <div className="tx-card-heading tx-observability-heading">
          <div>
            <p className="tx-section-label">{t('Telemetry')}</p>
            <h3>{t('Operation telemetry')}</h3>
            <p>{t('Browse sanitized MCP tool calls, timings, outcomes, and errors for this workspace.')}</p>
          </div>
          <Button size="sm" variant="outline-secondary" disabled={loading} onClick={refresh}>
            {loading ? <><Spinner animation="border" size="sm" /> {t('Refreshing…')}</> : t('Refresh')}
          </Button>
        </div>
        <Row className="g-3 tx-observability-toolbar">
          <Col sm={6} lg={3}>
            <Form.Group>
              <Form.Label>{t('Scope')}</Form.Label>
              <Form.Select value={filters.scope} onChange={event => setFilters(current => ({ ...current, scope: event.target.value as TelemetryScope }))}>
                <option value="current_runtime">{t('Current runtime')}</option>
                <option value="current_version">{t('Current version')}</option>
                <option value="all">{t('All retained')}</option>
              </Form.Select>
            </Form.Group>
          </Col>
          <Col sm={6} lg={2}>
            <Form.Group>
              <Form.Label>{t('Records')}</Form.Label>
              <Form.Select value={filters.limit} onChange={event => setFilters(current => ({ ...current, limit: Number(event.target.value) }))}>
                <option value={50}>50</option>
                <option value={100}>100</option>
                <option value={200}>200</option>
              </Form.Select>
            </Form.Group>
          </Col>
          <Col sm={6} lg={3}>
            <Form.Group>
              <Form.Label>{t('Sort by')}</Form.Label>
              <Form.Select value={filters.sortBy} onChange={event => setFilters(current => ({ ...current, sortBy: event.target.value as TelemetrySort }))}>
                <option value="calls">{t('Calls')}</option>
                <option value="errors">{t('Errors')}</option>
                <option value="duration_ms">{t('Duration')}</option>
                <option value="p95_ms">P95</option>
                <option value="queue_wait_ms">{t('Queue wait')}</option>
                <option value="request_bytes">{t('Request bytes')}</option>
                <option value="response_bytes">{t('Response bytes')}</option>
              </Form.Select>
            </Form.Group>
          </Col>
          <Col sm={6} lg={2}>
            <Form.Group>
              <Form.Label>{t('Minimum duration')}</Form.Label>
              <Form.Control type="number" min={0} max={86_400_000} value={filters.minDurationMs} onChange={event => setFilters(current => ({ ...current, minDurationMs: Number(event.target.value) || 0 }))} />
            </Form.Group>
          </Col>
          <Col sm={6} lg={2} className="d-flex align-items-end">
            <Form.Check type="switch" id={`telemetry-errors-${workspaceId}`} label={t('Errors only')} checked={filters.errorsOnly} onChange={event => setFilters(current => ({ ...current, errorsOnly: event.target.checked }))} />
          </Col>
        </Row>
      </article>

      {error ? <Alert variant="danger">{error}</Alert> : null}
      {data?.warnings.length ? <Alert variant="warning">{data.warnings.join(' · ')}</Alert> : null}

      <div className="tx-stat-grid">
        <article className="tx-stat-card"><span>{t('Calls')}</span><strong>{aggregate?.calls ?? 0}</strong><small>{data?.matched_lines ?? 0} / {data?.scanned_lines ?? 0} retained records</small></article>
        <article className="tx-stat-card"><span>{t('Errors')}</span><strong>{aggregate?.errors ?? 0}</strong><small>{data?.invalid_complete_lines ?? 0} invalid complete lines</small></article>
        <article className="tx-stat-card"><span>{t('Average duration')}</span><strong>{formatDuration(aggregate?.avg_ms ?? 0)}</strong><small>P50 {formatDuration(aggregate?.p50_ms ?? 0)}</small></article>
        <article className="tx-stat-card"><span>{t('P95 duration')}</span><strong>{formatDuration(aggregate?.p95_ms ?? 0)}</strong><small>Max {formatDuration(aggregate?.max_ms ?? 0)}</small></article>
      </div>

      <article className="tx-card">
        <div className="tx-card-heading"><div><p className="tx-section-label">{t('Tools')}</p><h3>{t('Telemetry aggregate')}</h3></div></div>
        <Table responsive hover className="align-middle mb-0 dashboard-table">
          <thead><tr><th>{t('Tool')}</th><th>{t('Calls')}</th><th>{t('Errors')}</th><th>Avg</th><th>P95</th><th>Max</th><th>Queue</th></tr></thead>
          <tbody>
            {aggregate?.tools.length ? aggregate.tools.map(item => (
              <tr key={item.tool}>
                <td><code>{item.tool}</code></td><td>{item.calls}</td><td>{item.errors}</td>
                <td>{formatDuration(item.avg_ms)}</td><td>{formatDuration(item.p95_ms)}</td><td>{formatDuration(item.max_ms)}</td><td>{formatDuration(item.queue_wait_ms)}</td>
              </tr>
            )) : <tr><td colSpan={7} className="text-center text-secondary py-4">{loading ? t('Loading telemetry…') : t('No telemetry records yet')}</td></tr>}
          </tbody>
        </Table>
      </article>

      <article className="tx-card">
        <div className="tx-card-heading">
          <div><p className="tx-section-label">{t('Recent operations')}</p><h3>{data?.records.length ?? 0} {t('Records')}</h3></div>
          {data?.matched_async_session_events ? <Badge bg="secondary">{data.matched_async_session_events} async sessions</Badge> : null}
        </div>
        <div className="tx-record-list">
          {data?.records.length ? data.records.slice().reverse().map((record, index) => (
            <details key={`${record.started_ts_ms ?? 0}-${record.tool ?? record.event ?? 'record'}-${index}`}>
              <summary>
                <span><code>{recordTitle(record)}</code><small>{record.started_ts_ms ? formatTime(record.started_ts_ms) : '—'}</small></span>
                <span><Badge bg={record.is_error || String(record.outcome).includes('error') ? 'danger' : 'success'}>{recordOutcome(record)}</Badge><small>{formatDuration(Number(record.duration_ms ?? 0))}</small></span>
              </summary>
              <pre className="tx-json">{JSON.stringify(record, null, 2)}</pre>
            </details>
          )) : <p className="tx-empty-state">{loading ? t('Loading telemetry…') : t('No telemetry records yet')}</p>}
        </div>
      </article>
    </div>
  );
}
