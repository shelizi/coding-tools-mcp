import { useCallback, useEffect, useState } from 'react';
import Alert from 'react-bootstrap/Alert';
import Badge from 'react-bootstrap/Badge';
import Button from 'react-bootstrap/Button';
import Spinner from 'react-bootstrap/Spinner';
import { runWorkspaceHealth } from '../api';
import { useI18n } from '../i18n';
import type { HealthCheckPayload } from '../types';

function errorText(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

export function HealthView({ workspaceId }: { workspaceId: string }) {
  const { t } = useI18n();
  const [data, setData] = useState<HealthCheckPayload | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');

  const run = useCallback(async (signal?: AbortSignal) => {
    setLoading(true);
    setError('');
    try {
      setData(await runWorkspaceHealth(workspaceId, signal));
    } catch (reason) {
      if (!signal?.aborted) setError(errorText(reason));
    } finally {
      if (!signal?.aborted) setLoading(false);
    }
  }, [workspaceId]);

  useEffect(() => {
    const controller = new AbortController();
    void run(controller.signal);
    return () => controller.abort();
  }, [run]);

  return (
    <article className="tx-card">
      <div className="tx-card-heading tx-observability-heading">
        <div>
          <p className="tx-section-label">{t('Health')}</p>
          <h3>{t('Health check')}</h3>
          <p>{t('Validate the local MCP listener, OAuth metadata, and optional Built-in WSS runtime.')}</p>
        </div>
        <Button size="sm" variant="outline-secondary" disabled={loading} onClick={() => void run()}>
          {loading ? <><Spinner animation="border" size="sm" /> {t('Checking…')}</> : t('Run health check')}
        </Button>
      </div>
      {error ? <Alert variant="danger" className="mt-3">{error}</Alert> : null}
      {data ? <Alert variant={data.ok ? 'success' : 'danger'} className="mt-3">{data.ok ? t('All required checks passed.') : t('One or more required checks failed.')}</Alert> : null}
      <div className="tx-health-list">
        {data?.items.map(item => (
          <div className="tx-health-item" key={item.id}>
            <div><strong>{item.label}</strong><p>{item.detail}</p>{item.hint ? <small>{item.hint}</small> : null}</div>
            <span><Badge bg={item.ok ? 'success' : 'danger'}>{item.ok ? t('Passed') : t('Failed')}</Badge>{!item.required ? <Badge bg="secondary">{t('Optional')}</Badge> : null}</span>
          </div>
        ))}
        {!data && !loading && !error ? <p className="tx-empty-state">{t('No health check has been run.')}</p> : null}
      </div>
    </article>
  );
}
