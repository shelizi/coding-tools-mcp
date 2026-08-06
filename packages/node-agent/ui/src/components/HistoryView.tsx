import { useCallback, useEffect, useRef, useState } from 'react';
import Alert from 'react-bootstrap/Alert';
import Badge from 'react-bootstrap/Badge';
import Button from 'react-bootstrap/Button';
import Form from 'react-bootstrap/Form';
import Spinner from 'react-bootstrap/Spinner';
import { fetchWorkspaceHistory, fetchWorkspaceHistorySession } from '../api';
import { useI18n } from '../i18n';
import type { HistoryCheckpoint, HistoryDetailPayload, HistoryListPayload, WorkspaceConfigSnapshot } from '../types';

function errorText(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function historyDate(value: string | null): string {
  if (!value) return '—';
  if (value.startsWith('unix:')) {
    const seconds = Number(value.slice(5));
    if (Number.isFinite(seconds)) return new Date(seconds * 1000).toLocaleString();
  }
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString();
}

function CheckpointSection({ label, items, code = false }: { label: string; items: string[]; code?: boolean }) {
  if (!items.length) return null;
  return <div className="tx-checkpoint-section"><h6>{label}</h6><ul className={code ? 'tx-code-list' : ''}>{items.map((item, index) => <li key={`${label}-${index}`}>{item}</li>)}</ul></div>;
}

function Checkpoint({ record }: { record: HistoryCheckpoint }) {
  const { t } = useI18n();
  return (
    <section className="tx-checkpoint">
      <header><code>{record.turnId}</code><span>{historyDate(record.timestamp)}</span></header>
      {record.userIntent ? <p className="tx-checkpoint-intent">{record.userIntent}</p> : null}
      <CheckpointSection label={t('Findings')} items={record.findings} />
      <CheckpointSection label={t('Decisions')} items={record.decisions} />
      <CheckpointSection label={t('Files changed')} items={record.filesChanged} code />
      <CheckpointSection label={t('Tests')} items={record.tests} />
      <CheckpointSection label={t('Runtime state')} items={record.runtimeState} />
      <CheckpointSection label={t('Remaining issues')} items={record.remainingIssues} />
      <CheckpointSection label={t('Next actions')} items={record.nextActions} />
      {record.notes ? <p className="tx-checkpoint-notes">{record.notes}</p> : null}
    </section>
  );
}

export function HistoryView({ workspace }: { workspace: WorkspaceConfigSnapshot }) {
  const { t } = useI18n();
  const [folderId, setFolderId] = useState(workspace.saved.folders[0]?.id ?? '');
  const [list, setList] = useState<HistoryListPayload | null>(null);
  const [detail, setDetail] = useState<HistoryDetailPayload | null>(null);
  const [selectedNumber, setSelectedNumber] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [detailLoading, setDetailLoading] = useState(false);
  const [error, setError] = useState('');
  const listRequest = useRef<AbortController | null>(null);
  const detailRequest = useRef<AbortController | null>(null);
  const selectedNumberRef = useRef<number | null>(null);

  const abortRequests = useCallback(() => {
    listRequest.current?.abort();
    detailRequest.current?.abort();
    listRequest.current = null;
    detailRequest.current = null;
  }, []);

  useEffect(() => {
    const available = workspace.saved.folders.some(folder => folder.id === folderId);
    if (!available) setFolderId(workspace.saved.folders[0]?.id ?? '');
  }, [workspace.saved.folders, folderId]);

  const loadDetail = useCallback(async (number: number) => {
    detailRequest.current?.abort();
    const controller = new AbortController();
    detailRequest.current = controller;
    selectedNumberRef.current = number;
    setSelectedNumber(number);
    setDetail(null);
    setDetailLoading(true);
    setError('');
    try {
      const selected = await fetchWorkspaceHistorySession(workspace.id, folderId, number, controller.signal);
      if (!controller.signal.aborted && selectedNumberRef.current === number) setDetail(selected);
    } catch (reason) {
      if (!controller.signal.aborted) {
        setError(errorText(reason));
        setDetail(null);
      }
    } finally {
      if (detailRequest.current === controller) {
        detailRequest.current = null;
        setDetailLoading(false);
      }
    }
  }, [folderId, workspace.id]);

  const loadHistory = useCallback(async (preferredNumber: number | null) => {
    if (!folderId) return;
    abortRequests();
    const controller = new AbortController();
    listRequest.current = controller;
    setLoading(true);
    setDetailLoading(false);
    setError('');
    try {
      const result = await fetchWorkspaceHistory(workspace.id, folderId, controller.signal);
      if (controller.signal.aborted) return;
      setList(result);
      const selected = preferredNumber !== null && result.sessions.some(session => session.number === preferredNumber)
        ? preferredNumber
        : result.sessions[0]?.number ?? null;
      if (selected === null) {
        selectedNumberRef.current = null;
        setSelectedNumber(null);
        setDetail(null);
      } else {
        void loadDetail(selected);
      }
    } catch (reason) {
      if (!controller.signal.aborted) {
        setError(errorText(reason));
        setDetail(null);
      }
    } finally {
      if (listRequest.current === controller) {
        listRequest.current = null;
        setLoading(false);
      }
    }
  }, [abortRequests, folderId, loadDetail, workspace.id]);

  useEffect(() => {
    if (!folderId || !workspace.saved.folders.some(folder => folder.id === folderId)) return;
    void loadHistory(null);
    return abortRequests;
  }, [abortRequests, folderId, loadHistory, workspace.saved.folders]);

  const selectSession = (number: number) => {
    if (number === selectedNumberRef.current && detail?.number === number) return;
    void loadDetail(number);
  };
  const integrityIssues = (list?.integrity.missingNumbers.length ?? 0)
    + (list?.integrity.invalidFiles.length ?? 0)
    + (list?.integrity.emptyFiles.length ?? 0)
    + (list?.integrity.duplicateSessionKeyCount ?? 0);

  return (
    <article className="tx-card">
      <div className="tx-card-heading tx-observability-heading">
        <div>
          <p className="tx-section-label">{t('History')}</p>
          <h3>{t('History sessions')}</h3>
          <p>{t('Browse saved development sessions and checkpoint records for this workspace folder.')}</p>
        </div>
        <div className="tx-history-controls">
          <Form.Select aria-label={t('History folder')} className="tx-history-folder" value={folderId} onChange={event => setFolderId(event.target.value)}>
            {workspace.saved.folders.map(folder => <option key={folder.id} value={folder.id}>{folder.name}</option>)}
          </Form.Select>
          <Button size="sm" variant="outline-secondary" disabled={loading} onClick={() => void loadHistory(selectedNumberRef.current)}>
            {loading ? <><Spinner animation="border" size="sm" /> {t('Refreshing…')}</> : t('Refresh')}
          </Button>
        </div>
      </div>

      {error ? <Alert variant="danger" className="mt-3">{error}</Alert> : null}
      {integrityIssues ? <Alert variant="warning" className="mt-3">{t('History integrity warnings')}: {integrityIssues}</Alert> : null}

      <div className="tx-history-layout">
        <nav className="tx-history-list" aria-label={t('History sessions')}>
          {loading ? <div className="tx-loading-inline"><Spinner animation="border" size="sm" /> {t('Loading history…')}</div> : null}
          {!loading && !list?.sessions.length ? <p className="tx-empty-state">{t('No history sessions yet')}</p> : null}
          {list?.sessions.map(session => (
            <Button
              key={session.number}
              type="button"
              variant="link"
              disabled={loading}
              aria-current={session.number === selectedNumber ? 'true' : undefined}
              className={session.number === selectedNumber ? 'tx-history-session active' : 'tx-history-session'}
              onClick={() => void selectSession(session.number)}
            >
              <span><strong>{session.title}</strong><Badge bg="secondary">#{session.number}</Badge></span>
              <small>{historyDate(session.updatedAt)} · {session.checkpointCount} {t('Checkpoints')}</small>
              <p>{session.summary}</p>
            </Button>
          ))}
        </nav>

        <div className="tx-history-detail" aria-live="polite">
          {detailLoading ? <div className="tx-loading-inline"><Spinner animation="border" size="sm" /> {t('Loading history…')}</div> : null}
          {!detailLoading && detail ? (
            <>
              <header>
                <div><p className="tx-section-label">{t('Session {number}', { number: detail.number })}</p><h3>{detail.title}</h3><code>{detail.path}</code></div>
                <Badge bg={detail.status === 'completed' ? 'success' : 'secondary'}>{detail.status}</Badge>
              </header>
              <div className="tx-history-meta"><span>{historyDate(detail.updatedAt)}</span><span>{detail.records.length} {t('Checkpoints')}</span></div>
              <div className="tx-checkpoint-list">
                {detail.records.length ? detail.records.map(record => <Checkpoint key={record.turnId} record={record} />) : <p className="tx-empty-state">{t('No checkpoints recorded in this session.')}</p>}
              </div>
              <details className="tx-raw-history"><summary>{t('View raw Markdown record')}</summary><pre className="tx-json">{detail.content}</pre></details>
            </>
          ) : null}
        </div>
      </div>
    </article>
  );
}
