import { useCallback, useEffect, useRef, useState } from 'react';
import Alert from 'react-bootstrap/Alert';
import Badge from 'react-bootstrap/Badge';
import Button from 'react-bootstrap/Button';
import Col from 'react-bootstrap/Col';
import Form from 'react-bootstrap/Form';
import Row from 'react-bootstrap/Row';
import Spinner from 'react-bootstrap/Spinner';
import { fetchWorkspaceOperationLogs } from '../api';
import { formatBytes, formatDuration, formatTime } from '../format';
import { useI18n } from '../i18n';
import type {
  OperationLogDiagnostics,
  OperationLogFilters,
  OperationLogItem,
  OperationLogPayload,
  OperationLogStatus,
  WorkspaceConfigSnapshot
} from '../types';

function errorText(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function statusVariant(status: OperationLogItem['status']): string {
  if (status === 'completed') return 'success';
  if (status === 'failed') return 'danger';
  return 'warning';
}

function operationTime(operation: OperationLogItem): number | null {
  return operation.finishedAt ?? operation.startedAt;
}

function hasDiagnostics(diagnostics: OperationLogDiagnostics): boolean {
  return Object.entries(diagnostics).some(([key, value]) => key === 'waitMs'
    ? Object.values(diagnostics.waitMs).some(wait => wait !== null)
    : value !== null);
}

function waitSummary(diagnostics: OperationLogDiagnostics): string {
  const labels: Record<keyof OperationLogDiagnostics['waitMs'], string> = {
    blocking: 'blocking',
    workspaceAdmission: 'workspace admission',
    globalAdmission: 'global admission',
    admissionQueue: 'admission queue',
    workspaceLock: 'workspace lock',
    operationLock: 'operation lock',
    resourceLock: 'resource lock',
    historyLock: 'history lock',
    sessionRegistry: 'session registry'
  };
  const waits = Object.entries(diagnostics.waitMs)
    .filter((entry): entry is [keyof OperationLogDiagnostics['waitMs'], number] => entry[1] !== null && entry[1] > 0)
    .map(([key, value]) => `${labels[key]} ${formatDuration(value)}`);
  return waits.length ? waits.join(' · ') : '—';
}

export function OperationLogView({ workspace }: { workspace: WorkspaceConfigSnapshot }) {
  const { t } = useI18n();
  const folders = workspace.effective.folders;
  const firstFolderId = folders[0]?.id ?? '';
  const [filters, setFilters] = useState<OperationLogFilters>({
    folderId: firstFolderId,
    status: 'all',
    tool: '',
    errorsOnly: false,
    limit: 50
  });
  const [data, setData] = useState<OperationLogPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [error, setError] = useState('');
  const requestRef = useRef<AbortController | null>(null);

  useEffect(() => {
    const available = folders.some(folder => folder.id === filters.folderId);
    if (!available) setFilters(current => ({ ...current, folderId: firstFolderId }));
  }, [filters.folderId, firstFolderId, folders]);

  const load = useCallback(async (cursor: number, append: boolean) => {
    if (!filters.folderId) return;
    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;
    if (append) setLoadingOlder(true);
    else setLoading(true);
    setError('');
    try {
      const result = await fetchWorkspaceOperationLogs(workspace.id, filters, cursor, controller.signal);
      if (controller.signal.aborted || requestRef.current !== controller) return;
      setData(current => append && current
        ? { ...result, operations: [...current.operations, ...result.operations] }
        : result);
    } catch (reason) {
      if (!controller.signal.aborted) setError(errorText(reason));
    } finally {
      if (requestRef.current === controller) {
        requestRef.current = null;
        setLoading(false);
        setLoadingOlder(false);
      }
    }
  }, [filters, workspace.id]);

  useEffect(() => {
    if (!filters.folderId) return;
    void load(0, false);
    return () => requestRef.current?.abort();
  }, [filters.folderId, filters.status, filters.tool, filters.errorsOnly, filters.limit, load]);

  const setTool = (value: string) => {
    if (/^[A-Za-z0-9._-]*$/.test(value)) setFilters(current => ({ ...current, tool: value }));
  };

  return (
    <div className="tx-observability-stack">
      <article className="tx-card">
        <div className="tx-card-heading tx-observability-heading">
          <div>
            <p className="tx-section-label">{t('Logs')}</p>
            <h3>{t('Operation log')}</h3>
            <p>{t('Browse persisted operation starts, completions, failures, and interrupted records without exposing commands or output.')}</p>
          </div>
          <Button size="sm" variant="outline-secondary" disabled={loading} onClick={() => void load(0, false)}>
            {loading ? <><Spinner animation="border" size="sm" /> {t('Refreshing…')}</> : t('Refresh')}
          </Button>
        </div>
        <Row className="g-3 tx-observability-toolbar">
          <Col sm={6} lg={3}>
            <Form.Group>
              <Form.Label>{t('Log folder')}</Form.Label>
              <Form.Select value={filters.folderId} onChange={event => setFilters(current => ({ ...current, folderId: event.target.value }))}>
                {folders.map(folder => <option key={folder.id} value={folder.id}>{folder.name}</option>)}
              </Form.Select>
            </Form.Group>
          </Col>
          <Col sm={6} lg={2}>
            <Form.Group>
              <Form.Label>{t('Status')}</Form.Label>
              <Form.Select value={filters.status} onChange={event => setFilters(current => ({ ...current, status: event.target.value as OperationLogStatus }))}>
                <option value="all">{t('All statuses')}</option>
                <option value="completed">{t('Completed')}</option>
                <option value="failed">{t('Failed')}</option>
                <option value="incomplete">{t('Incomplete')}</option>
              </Form.Select>
            </Form.Group>
          </Col>
          <Col sm={6} lg={3}>
            <Form.Group>
              <Form.Label>{t('Tool filter')}</Form.Label>
              <Form.Control value={filters.tool} placeholder="exec_command" onChange={event => setTool(event.target.value)} />
            </Form.Group>
          </Col>
          <Col sm={6} lg={2}>
            <Form.Group>
              <Form.Label>{t('Records')}</Form.Label>
              <Form.Select value={filters.limit} onChange={event => setFilters(current => ({ ...current, limit: Number(event.target.value) }))}>
                <option value={25}>25</option>
                <option value={50}>50</option>
                <option value={100}>100</option>
                <option value={200}>200</option>
              </Form.Select>
            </Form.Group>
          </Col>
          <Col sm={6} lg={2} className="d-flex align-items-end">
            <Form.Check
              type="switch"
              id={`operation-errors-${workspace.id}`}
              label={t('Failures and incomplete only')}
              checked={filters.errorsOnly}
              onChange={event => setFilters(current => ({ ...current, errorsOnly: event.target.checked }))}
            />
          </Col>
        </Row>
      </article>

      {error ? <Alert variant="danger">{error}</Alert> : null}

      <div className="tx-stat-grid">
        <article className="tx-stat-card"><span>{t('Operations')}</span><strong>{data?.summary.total ?? 0}</strong><small>{data?.matched ?? 0} {t('matched')}</small></article>
        <article className="tx-stat-card"><span>{t('Completed')}</span><strong>{data?.summary.completed ?? 0}</strong><small>{t('Terminal success records')}</small></article>
        <article className="tx-stat-card"><span>{t('Failed')}</span><strong>{data?.summary.failed ?? 0}</strong><small>{t('Terminal failure records')}</small></article>
        <article className="tx-stat-card"><span>{t('Incomplete')}</span><strong>{data?.summary.incomplete ?? 0}</strong><small>{t('Started without a terminal record')}</small></article>
      </div>

      <article className="tx-card">
        <div className="tx-card-heading">
          <div><p className="tx-section-label">{t('Recent operations')}</p><h3>{data?.operations.length ?? 0} / {data?.matched ?? 0} {t('Records')}</h3></div>
          {data?.folder.name ? <Badge bg="secondary">{data.folder.name}</Badge> : null}
        </div>
        <div className="tx-record-list tx-operation-list" aria-live="polite">
          {data?.operations.length ? data.operations.map(operation => (
            <details key={operation.id}>
              <summary>
                <span><code>{operation.tool}</code><small>{formatTime(operationTime(operation))}</small></span>
                <span><Badge bg={statusVariant(operation.status)}>{t(operation.status === 'completed' ? 'Completed' : operation.status === 'failed' ? 'Failed' : 'Incomplete')}</Badge><small>{operation.durationMs === null ? '—' : formatDuration(operation.durationMs)}</small></span>
              </summary>
              <div className="tx-operation-detail">
                {operation.status === 'incomplete' ? <Alert variant="warning" className="mb-0">{t('This operation started but has no terminal record. The Agent may have stopped or restarted before completion.')}</Alert> : null}
                <dl className="tx-operation-meta">
                  <div><dt>{t('Correlation ID')}</dt><dd><code>{operation.id}</code></dd></div>
                  <div><dt>{t('Tracked task')}</dt><dd>{operation.taskTracked ? t('Yes') : t('No')}</dd></div>
                  <div><dt>{t('Affected files')}</dt><dd>{operation.affectedFileCount}</dd></div>
                  <div><dt>{t('Duration')}</dt><dd>{operation.durationMs === null ? '—' : formatDuration(operation.durationMs)}</dd></div>
                </dl>
                {hasDiagnostics(operation.diagnostics) ? (
                  <dl className="tx-operation-meta tx-operation-diagnostics">
                    <div><dt>{t('Error')}</dt><dd>{[operation.diagnostics.errorCode, operation.diagnostics.errorCategory].filter(Boolean).join(' · ') || '—'}</dd></div>
                    <div><dt>{t('Command result')}</dt><dd>{operation.diagnostics.commandOk === null ? '—' : operation.diagnostics.commandOk ? t('Succeeded') : t('Failed')}</dd></div>
                    <div><dt>{t('Verification')}</dt><dd>{operation.diagnostics.verificationOk === null ? '—' : operation.diagnostics.verificationOk ? t('Passed') : t('Failed')}</dd></div>
                    <div><dt>{t('Runtime result')}</dt><dd>{[operation.diagnostics.runtimeStatus, operation.diagnostics.terminationReason].filter(Boolean).join(' · ') || '—'}</dd></div>
                    <div><dt>{t('Exit code')}</dt><dd>{operation.diagnostics.exitCode ?? '—'}</dd></div>
                    <div><dt>{t('Elapsed')}</dt><dd>{operation.diagnostics.elapsedMs === null ? '—' : formatDuration(operation.diagnostics.elapsedMs)}</dd></div>
                    <div><dt>{t('First output')}</dt><dd>{operation.diagnostics.firstOutputMs === null ? '—' : formatDuration(operation.diagnostics.firstOutputMs)}</dd></div>
                    <div><dt>{t('Warnings')}</dt><dd>{operation.diagnostics.warningCount ?? 0}</dd></div>
                    <div><dt>{t('Timeouts')}</dt><dd>{operation.diagnostics.processTimedOut ? t('Process') : operation.diagnostics.requestTimedOut ? t('Request') : '—'}</dd></div>
                    <div><dt>{t('Retryable')}</dt><dd>{operation.diagnostics.retryable === null ? '—' : operation.diagnostics.retryable ? t('Yes') : t('No')}</dd></div>
                    <div><dt>{t('Output size')}</dt><dd>{operation.diagnostics.stdoutBytes === null && operation.diagnostics.stderrBytes === null ? '—' : `${formatBytes(operation.diagnostics.stdoutBytes ?? 0)} / ${formatBytes(operation.diagnostics.stderrBytes ?? 0)}`}</dd></div>
                    <div className="tx-operation-meta-wide"><dt>{t('Wait time')}</dt><dd>{waitSummary(operation.diagnostics)}</dd></div>
                  </dl>
                ) : null}
                {operation.reason ? <p className="tx-operation-reason"><strong>{t('Reason')}</strong>{operation.reason}</p> : null}
                <div className="tx-operation-events">
                  <h6>{t('Event timeline')}</h6>
                  <ol>{operation.events.map((event, index) => <li key={`${event.kind}-${event.createdAt}-${index}`}><Badge bg={event.kind === 'failed' ? 'danger' : event.kind === 'completed' ? 'success' : 'secondary'}>{event.kind}</Badge><span>{formatTime(event.createdAt)}</span></li>)}</ol>
                </div>
              </div>
            </details>
          )) : <p className="tx-empty-state">{loading ? t('Loading operation logs…') : t('No operation logs yet')}</p>}
        </div>
        {data?.nextCursor !== null && data?.nextCursor !== undefined ? (
          <div className="tx-operation-load">
            <Button variant="outline-secondary" disabled={loadingOlder} onClick={() => void load(data.nextCursor ?? 0, true)}>
              {loadingOlder ? <><Spinner animation="border" size="sm" /> {t('Loading older…')}</> : t('Load older')}
            </Button>
          </div>
        ) : null}
      </article>
    </div>
  );
}
