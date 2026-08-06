import { useState, type KeyboardEvent } from 'react';
import Alert from 'react-bootstrap/Alert';
import Badge from 'react-bootstrap/Badge';
import Button from 'react-bootstrap/Button';
import Spinner from 'react-bootstrap/Spinner';
import Table from 'react-bootstrap/Table';
import { formatDuration, formatTime } from '../format';
import { useI18n } from '../i18n';
import { fetchWorkspaceDiagnostics } from '../api';
import type {
  ConfigSaveResult,
  ConfigUpdatePayload,
  DashboardPayload,
  ManagementStatus,
  SecretResult,
  WorkspaceConfigSnapshot
} from '../types';
import { ConfigForm } from './ConfigForm';
import { CopyField } from './CopyField';
import { ActivityIcon, DownloadIcon, FolderIcon, HeartPulseIcon, HistoryIcon, LogsIcon, PlugIcon, SettingsIcon, ShieldIcon } from './Icons';
import { HealthView } from './HealthView';
import { HistoryView } from './HistoryView';
import { OperationalSummary } from './OperationalSummary';
import { OperationLogView } from './OperationLogView';
import { TelemetryView } from './TelemetryView';

export type WorkspaceTab = 'overview' | 'history' | 'telemetry' | 'logs' | 'health' | 'settings';

const WORKSPACE_TABS: WorkspaceTab[] = ['overview', 'history', 'telemetry', 'logs', 'health', 'settings'];

interface WorkspaceViewProps {
  workspace: WorkspaceConfigSnapshot;
  status: ManagementStatus;
  dashboard: DashboardPayload;
  password: string;
  passwordLoading: boolean;
  passwordError?: unknown;
  saving: boolean;
  regeneratingPassword: boolean;
  activeTab: WorkspaceTab;
  onTabChange(tab: WorkspaceTab): void;
  onSave(payload: ConfigUpdatePayload): Promise<ConfigSaveResult>;
  onRegeneratePassword(): Promise<SecretResult>;
  onQuickSetup(): void;
}

function errorText(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

export function WorkspaceView({
  workspace,
  status,
  dashboard,
  password,
  passwordLoading,
  passwordError,
  saving,
  regeneratingPassword,
  activeTab,
  onTabChange,
  onSave,
  onRegeneratePassword,
  onQuickSetup
}: WorkspaceViewProps) {
  const { t } = useI18n();
  const displayConfig = workspace.restartRequired ? workspace.saved : workspace.effective;
  const runtime = status.workspaces.find(item => item.id === workspace.id);
  const publicEndpoint = displayConfig.tunnel.publicUrl;
  const activity = dashboard.activity.slice(0, 20);
  const [exportingDiagnostics, setExportingDiagnostics] = useState(false);
  const [diagnosticsError, setDiagnosticsError] = useState('');

  const downloadDiagnostics = async () => {
    setExportingDiagnostics(true);
    setDiagnosticsError('');
    try {
      const diagnostics = await fetchWorkspaceDiagnostics(workspace.id);
      const blob = new Blob([JSON.stringify(diagnostics, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = `coding-tools-${workspace.id}-diagnostics.json`;
      anchor.click();
      URL.revokeObjectURL(url);
    } catch (error) {
      setDiagnosticsError(errorText(error));
    } finally {
      setExportingDiagnostics(false);
    }
  };

  const tabId = (tab: WorkspaceTab) => `workspace-${workspace.id}-tab-${tab}`;
  const panelId = (tab: WorkspaceTab) => `workspace-${workspace.id}-panel-${tab}`;
  const handleTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>, current: WorkspaceTab) => {
    const currentIndex = WORKSPACE_TABS.indexOf(current);
    let nextIndex: number | null = null;
    if (event.key === 'ArrowRight') nextIndex = (currentIndex + 1) % WORKSPACE_TABS.length;
    if (event.key === 'ArrowLeft') nextIndex = (currentIndex - 1 + WORKSPACE_TABS.length) % WORKSPACE_TABS.length;
    if (event.key === 'Home') nextIndex = 0;
    if (event.key === 'End') nextIndex = WORKSPACE_TABS.length - 1;
    if (nextIndex === null) return;
    event.preventDefault();
    const next = WORKSPACE_TABS[nextIndex];
    onTabChange(next);
    document.getElementById(tabId(next))?.focus();
  };

  return (
    <section className="tx-page">
      <header className="tx-page-header">
        <div className="tx-workspace-header">
          <div className="tx-title-icon"><FolderIcon width={24} height={24} /></div>
          <div>
            <p className="tx-page-kicker">{t('Workspace')}</p>
            <h2>{workspace.name}</h2>
            <p>{displayConfig.folders.length} {t('Folders')} · {displayConfig.host}:{displayConfig.port}</p>
          </div>
        </div>
        <div className="tx-workspace-actions">
          <nav className="tx-workspace-tabs" aria-label={t('Workspace')} role="tablist">
            <button id={tabId('overview')} type="button" role="tab" aria-selected={activeTab === 'overview'} aria-controls={panelId('overview')} tabIndex={activeTab === 'overview' ? 0 : -1} className={activeTab === 'overview' ? 'active' : ''} onKeyDown={event => handleTabKeyDown(event, 'overview')} onClick={() => onTabChange('overview')}>
              <PlugIcon width={16} height={16} />{t('Overview')}
            </button>
            <button id={tabId('history')} type="button" role="tab" aria-selected={activeTab === 'history'} aria-controls={panelId('history')} tabIndex={activeTab === 'history' ? 0 : -1} className={activeTab === 'history' ? 'active' : ''} onKeyDown={event => handleTabKeyDown(event, 'history')} onClick={() => onTabChange('history')}>
              <HistoryIcon width={16} height={16} />{t('History')}
            </button>
            <button id={tabId('telemetry')} type="button" role="tab" aria-selected={activeTab === 'telemetry'} aria-controls={panelId('telemetry')} tabIndex={activeTab === 'telemetry' ? 0 : -1} className={activeTab === 'telemetry' ? 'active' : ''} onKeyDown={event => handleTabKeyDown(event, 'telemetry')} onClick={() => onTabChange('telemetry')}>
              <ActivityIcon width={16} height={16} />{t('Telemetry')}
            </button>
            <button id={tabId('logs')} type="button" role="tab" aria-selected={activeTab === 'logs'} aria-controls={panelId('logs')} tabIndex={activeTab === 'logs' ? 0 : -1} className={activeTab === 'logs' ? 'active' : ''} onKeyDown={event => handleTabKeyDown(event, 'logs')} onClick={() => onTabChange('logs')}>
              <LogsIcon width={16} height={16} />{t('Logs')}
            </button>
            <button id={tabId('health')} type="button" role="tab" aria-selected={activeTab === 'health'} aria-controls={panelId('health')} tabIndex={activeTab === 'health' ? 0 : -1} className={activeTab === 'health' ? 'active' : ''} onKeyDown={event => handleTabKeyDown(event, 'health')} onClick={() => onTabChange('health')}>
              <HeartPulseIcon width={16} height={16} />{t('Health')}
            </button>
            <button id={tabId('settings')} type="button" role="tab" aria-selected={activeTab === 'settings'} aria-controls={panelId('settings')} tabIndex={activeTab === 'settings' ? 0 : -1} className={activeTab === 'settings' ? 'active' : ''} onKeyDown={event => handleTabKeyDown(event, 'settings')} onClick={() => onTabChange('settings')}>
              <SettingsIcon width={16} height={16} />{t('Settings')}
            </button>
          </nav>
          <Button size="sm" variant="outline-secondary" disabled={exportingDiagnostics} onClick={() => void downloadDiagnostics()}>
            <DownloadIcon width={15} height={15} /> {exportingDiagnostics ? t('Exporting…') : t('Export diagnostics')}
          </Button>
        </div>
      </header>

      <div className="tx-page-body" id={panelId(activeTab)} role="tabpanel" aria-labelledby={tabId(activeTab)} tabIndex={0}>
        {workspace.restartRequired ? (
          <Alert variant="warning">
            <strong>{t('Configuration pending restart')}</strong>
            <div>{t('Restart the Agent to apply the saved tunnel and OAuth settings.')}</div>
          </Alert>
        ) : null}

        {diagnosticsError ? <Alert variant="danger">{diagnosticsError}</Alert> : null}

        {activeTab === 'settings' ? (
          <ConfigForm snapshot={workspace} saving={saving} onSave={onSave} />
        ) : activeTab === 'history' ? (
          <HistoryView workspace={workspace} />
        ) : activeTab === 'telemetry' ? (
          <TelemetryView workspaceId={workspace.id} />
        ) : activeTab === 'logs' ? (
          <OperationLogView workspace={workspace} />
        ) : activeTab === 'health' ? (
          <HealthView workspaceId={workspace.id} />
        ) : (
          <>
            <div className="tx-stat-grid">
              <article className="tx-stat-card"><FolderIcon width={21} height={21} /><span>{t('Folders')}</span><strong>{displayConfig.folders.length}</strong><small>{workspace.name}</small></article>
              <article className="tx-stat-card"><ShieldIcon width={21} height={21} /><span>{t('MCP policy')}</span><strong>{runtime?.permissionMode ?? displayConfig.permissionMode}</strong><small>{runtime?.toolProfile ?? displayConfig.activeToolProfile}</small></article>
              <article className="tx-stat-card"><PlugIcon width={21} height={21} /><span>Port</span><strong>{displayConfig.port}</strong><small>{displayConfig.host}</small></article>
              <article className="tx-stat-card"><ShieldIcon width={21} height={21} /><span>{t('Built-in WSS tunnel')}</span><strong>{runtime?.tunnel?.state ?? 'disabled'}</strong><small>{displayConfig.tunnel.enabled ? t('Running') : t('Not available')}</small></article>
            </div>

            <div className="tx-content-grid">
              <article className="tx-card tx-gpt-card">
                <div className="tx-card-heading">
                  <div>
                    <p className="tx-section-label">{t('GPT configuration')}</p>
                    <h3>{t('Copy these values to ChatGPT → Settings → Connectors / MCP')}</h3>
                  </div>
                  <Button size="sm" variant="outline-primary" onClick={onQuickSetup}>{t('Quick setup')}</Button>
                </div>
                <div className="tx-copy-stack">
                  {publicEndpoint ? (
                    <CopyField label={t('Public MCP endpoint')} value={publicEndpoint} hint={t('Enter this URL in the GPT connector')} />
                  ) : (
                    <Alert variant="secondary" className="mb-0">
                      <strong>{t('No public MCP endpoint is configured.')}</strong>
                      <div className="mt-2">{t('Use Quick setup to register this Agent with Built-in WSS.')}</div>
                    </Alert>
                  )}
                  <CopyField label="OAuth Client ID" value={displayConfig.oauth.clientId} />
                  {passwordLoading ? (
                    <div className="tx-secret-loading"><Spinner animation="border" size="sm" />{t('Loading authorization password…')}</div>
                  ) : passwordError ? (
                    <Alert variant="danger" className="mb-0">{errorText(passwordError)}</Alert>
                  ) : (
                    <CopyField label={t('Authorization password')} value={password} hint={t('Available anytime from this workspace overview.')} secret />
                  )}
                  <div>
                    <Button
                      type="button"
                      size="sm"
                      variant="outline-danger"
                      disabled={regeneratingPassword}
                      onClick={() => void onRegeneratePassword()}
                    >
                      {regeneratingPassword ? t('Generating…') : t('Generate another password')}
                    </Button>
                  </div>
                </div>
              </article>

              <article className="tx-card">
                <div className="tx-card-heading">
                  <div><p className="tx-section-label">{t('Folders')}</p><h3>{workspace.name}</h3></div>
                  <Button size="sm" variant="outline-secondary" onClick={() => onTabChange('settings')}>{t('Edit folders')}</Button>
                </div>
                <div className="tx-folder-list">
                  {displayConfig.folders.map(folder => (
                    <div key={folder.id}>
                      <FolderIcon width={17} height={17} />
                      <span><strong>{folder.name}</strong><code>{folder.path}</code><small>{folder.id}</small></span>
                    </div>
                  ))}
                </div>
              </article>
            </div>

            <OperationalSummary dashboard={dashboard} />

            <article className="tx-card mt-4">
              <div className="tx-card-heading">
                <div><p className="tx-section-label">{t('Recent activity')}</p><h3>{workspace.name}</h3></div>
              </div>
              <Table responsive hover className="align-middle mb-0 dashboard-table">
                <thead><tr><th>{t('Time')}</th><th>Tool</th><th>{t('Result')}</th><th>{t('Duration')}</th></tr></thead>
                <tbody>
                  {activity.length ? activity.map((item, index) => (
                    <tr key={`${item.startedAt}-${item.tool}-${index}`}>
                      <td>{formatTime(item.startedAt)}</td>
                      <td><code>{item.tool}</code></td>
                      <td><Badge bg={item.ok ? 'success' : 'danger'}>{item.ok ? t('Success') : t('Failed')}</Badge></td>
                      <td>{formatDuration(item.durationMs)}</td>
                    </tr>
                  )) : <tr><td colSpan={4} className="text-center text-secondary py-4">{t('No recent activity for this workspace.')}</td></tr>}
                </tbody>
              </Table>
            </article>
          </>
        )}
      </div>
    </section>
  );
}
