import Badge from 'react-bootstrap/Badge';
import type { DashboardPayload } from '../types';
import { formatTime } from '../format';
import { useI18n } from '../i18n';

export function OperationalSummary({ dashboard }: { dashboard: DashboardPayload }) {
  const { t } = useI18n();
  const persistent = dashboard.usage.persistent;
  const tunnel = dashboard.tunnel;
  return (
    <article className="tx-card mt-4">
      <div className="tx-card-heading"><div><p className="tx-section-label">{t('Operational details')}</p><h3>{t('Dashboard contract')}</h3></div></div>
      <div className="tx-operational-grid">
        <div className="tx-operational-item">
          <span>{t('Pending permissions')}</span><strong>{dashboard.permissions.pending}</strong>
          <small>{dashboard.permissions.byWorkspace.map(item => `${item.workspaceFolderId}: ${item.pending}`).join(' · ') || t('None')}</small>
        </div>
        <div className="tx-operational-item">
          <span>{t('Persistent telemetry')}</span><strong>{persistent.matchedLines ?? 0}</strong>
          <small>{persistent.scannedLines ?? 0} scanned · {persistent.invalidCompleteLines ?? 0} invalid</small>
        </div>
        <div className="tx-operational-item">
          <span>{t('Tunnel workers')}</span><strong>{tunnel.connectedWorkers ?? 0}/{tunnel.workers ?? 0}</strong>
          <small>{tunnel.idleWorkers ?? 0} idle · {tunnel.busyWorkers ?? 0} busy · {tunnel.recycledWorkers ?? 0} recycled</small>
        </div>
        <div className="tx-operational-item">
          <span>{t('Tunnel requests')}</span><strong>{tunnel.completedRequests ?? 0}</strong>
          <small>{t('Last request timeout')}: {tunnel.lastRequestTimeout ? <><Badge bg="warning">{tunnel.lastRequestTimeout}</Badge> {tunnel.lastRequestTimeoutAt ? formatTime(tunnel.lastRequestTimeoutAt) : ''}</> : t('Never')}</small>
        </div>
      </div>
    </article>
  );
}
