import { useEffect, useState } from 'react';
import Alert from 'react-bootstrap/Alert';
import Badge from 'react-bootstrap/Badge';
import Button from 'react-bootstrap/Button';
import Form from 'react-bootstrap/Form';
import Spinner from 'react-bootstrap/Spinner';
import { DataTables } from './components/DataTables';
import {
  BoltIcon,
  FolderIcon,
  GaugeIcon,
  MenuIcon,
  MoonIcon,
  RefreshIcon,
  SunIcon,
  XIcon
} from './components/Icons';
import { OverviewCards } from './components/OverviewCards';
import { QuickSetup } from './components/QuickSetup';
import { WorkspaceView, type WorkspaceTab } from './components/WorkspaceView';
import { formatTime } from './format';
import { useAgentQueries } from './hooks/useAgentQueries';
import { usePwaInstall } from './hooks/usePwaInstall';
import { useTheme } from './hooks/useTheme';
import { useI18n } from './i18n';

type View = 'dashboard' | 'quick-setup' | 'workspace';

function errorText(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

function delay(milliseconds: number): Promise<void> {
  return new Promise(resolve => window.setTimeout(resolve, milliseconds));
}

async function reloadWhenAgentReturns(): Promise<void> {
  const startedAt = Date.now();
  const deadline = startedAt + 30_000;
  let observedOffline = false;
  await delay(300);
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`/health?restart=${Date.now()}`, { cache: 'no-store' });
      if (response.ok && (observedOffline || Date.now() - startedAt >= 2_500)) {
        window.location.reload();
        return;
      }
    } catch {
      observedOffline = true;
    }
    await delay(300);
  }
  window.location.reload();
}

export default function App() {
  const [view, setView] = useState<View>('dashboard');
  const [selectedWorkspaceId, setSelectedWorkspaceId] = useState('');
  const [workspaceTab, setWorkspaceTab] = useState<WorkspaceTab>('overview');
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [restartError, setRestartError] = useState('');
  const agent = useAgentQueries(selectedWorkspaceId);
  const pwa = usePwaInstall();
  const theme = useTheme();
  const { locale, setLocale, options, t } = useI18n();
  const status = agent.status.data;
  const dashboard = agent.dashboard.data;
  const config = agent.config.data;
  const firstError = agent.status.error ?? agent.dashboard.error ?? agent.config.error;

  useEffect(() => {
    if (!config?.workspaces.length) return;
    if (!config.workspaces.some(workspace => workspace.id === selectedWorkspaceId)) {
      setSelectedWorkspaceId(config.primaryWorkspaceId || config.workspaces[0].id);
    }
  }, [config, selectedWorkspaceId]);

  if (!status || !dashboard || !config) {
    return (
      <main className="loading-screen">
        {firstError ? (
          <Alert variant="danger" className="loading-error">
            <Alert.Heading>{t('Management UI failed to load')}</Alert.Heading>
            <p>{errorText(firstError)}</p>
            <Button variant="outline-danger" onClick={() => void agent.refresh()}>{t('Retry')}</Button>
          </Alert>
        ) : (
          <div className="tx-loading-row">
            <Spinner animation="border" role="status" />
            <span>{t('Loading Agent status…')}</span>
          </div>
        )}
      </main>
    );
  }

  const selectedWorkspace = config.workspaces.find(workspace => workspace.id === selectedWorkspaceId)
    ?? config.workspaces.find(workspace => workspace.id === config.primaryWorkspaceId)
    ?? config.workspaces[0];
  const selectedRuntime = status.workspaces.find(workspace => workspace.id === selectedWorkspace?.id);
  const restartRequired = config.workspaces.some(workspace => workspace.restartRequired);
  const password = agent.regeneratePassword.data?.workspaceId === selectedWorkspace?.id
    ? agent.regeneratePassword.data.value
    : agent.password.data?.value ?? '';

  const navigate = (next: View) => {
    setView(next);
    setSidebarOpen(false);
  };

  const openWorkspace = (workspaceId: string, tab: WorkspaceTab = 'overview') => {
    setSelectedWorkspaceId(workspaceId);
    setWorkspaceTab(tab);
    navigate('workspace');
  };

  const restart = async () => {
    if (!window.confirm(t('Restart the Agent now? Active tool calls and command sessions will be stopped.'))) return;
    setRestartError('');
    setRestarting(true);
    try {
      await agent.restart.mutateAsync();
      await reloadWhenAgentReturns();
    } catch (error) {
      setRestarting(false);
      setRestartError(errorText(error));
    }
  };

  const sidebar = (
    <aside className={`tx-sidebar ${sidebarOpen ? 'open' : ''}`}>
      <div className="tx-sidebar-header">
        <div className="tx-brand-row">
          <div>
            <p className="tx-brand-kicker">Coding Tools</p>
            <h1>{t('Desktop Console')}</h1>
          </div>
          <button type="button" className="tx-sidebar-close" aria-label={t('Close')} onClick={() => setSidebarOpen(false)}>
            <XIcon width={20} height={20} />
          </button>
        </div>
        <div className="tx-sidebar-controls">
          <Form.Select size="sm" aria-label={t('Language')} value={locale} onChange={event => setLocale(event.target.value as typeof locale)}>
            {options.map(option => <option key={option.value} value={option.value}>{option.label}</option>)}
          </Form.Select>
          <button
            type="button"
            className="tx-icon-button"
            aria-label={theme.resolved === 'dark' ? t('Light theme') : t('Dark theme')}
            onClick={() => theme.setPreference(theme.resolved === 'dark' ? 'light' : 'dark')}
          >
            {theme.resolved === 'dark' ? <SunIcon width={18} height={18} /> : <MoonIcon width={18} height={18} />}
          </button>
        </div>
        <button
          type="button"
          className={`tx-primary-nav ${view === 'quick-setup' ? 'active' : ''}`}
          disabled={!selectedWorkspace}
          onClick={() => navigate('quick-setup')}
        >
          <BoltIcon width={18} height={18} />
          {t('Quick setup')}
        </button>
      </div>

      <div className="tx-sidebar-body">
        <p className="tx-sidebar-section-label">{t('Workspaces')}</p>
        <nav className="tx-workspace-nav" aria-label={t('Workspaces')}>
          {config.workspaces.map(workspace => {
            const saved = workspace.saved;
            return (
              <button
                type="button"
                key={workspace.id}
                className={view === 'workspace' && selectedWorkspace?.id === workspace.id ? 'active' : ''}
                onClick={() => openWorkspace(workspace.id)}
              >
                <FolderIcon width={17} height={17} />
                <span>
                  <strong>{workspace.name}</strong>
                  <small>{saved.folders.length} {t('Folders')} · {saved.host}:{saved.port}</small>
                </span>
              </button>
            );
          })}
        </nav>
      </div>

      <div className="tx-sidebar-footer">
        <button type="button" className={view === 'dashboard' ? 'active' : ''} onClick={() => navigate('dashboard')}>
          <GaugeIcon width={17} height={17} />{t('Dashboard')}
        </button>
        <p className="tx-app-version">Node Agent v{status.version}</p>
      </div>
    </aside>
  );

  return (
    <div className="app-shell">
      {restarting ? (
        <div className="tx-restart-overlay" role="status" aria-live="assertive">
          <div className="tx-restart-dialog">
            <Spinner animation="border" />
            <h2>{t('Restarting Agent…')}</h2>
            <p>{t('The Agent is restarting. This page will reconnect automatically.')}</p>
          </div>
        </div>
      ) : null}
      {sidebar}
      {sidebarOpen ? <button type="button" className="tx-sidebar-backdrop" aria-label={t('Close')} onClick={() => setSidebarOpen(false)} /> : null}

      <main className="tx-main">
        <div className="tx-topbar">
          <button type="button" className="tx-mobile-menu" aria-label={t('Menu')} onClick={() => setSidebarOpen(true)}>
            <MenuIcon width={20} height={20} />
          </button>
          <div className="tx-topbar-spacer" />
          <Badge bg={restartRequired ? 'warning' : 'success'} text={restartRequired ? 'dark' : undefined}>
            {restartRequired ? t('Restart required') : t('Running')}
          </Badge>
          {pwa.canInstall ? <Button size="sm" variant="outline-primary" onClick={() => void pwa.install()}>{t('Install UI')}</Button> : null}
          <Button
            size="sm"
            variant={restartRequired ? 'warning' : 'outline-danger'}
            disabled={!status.restart.supported || agent.restart.isPending || restarting}
            title={status.restart.supported ? t('Restart Agent') : t('Restart is available when launched with start-node-agent.bat.')}
            onClick={() => void restart()}
          >
            <RefreshIcon width={15} height={15} />
            {agent.restart.isPending ? t('Restarting Agent…') : t('Restart Agent')}
          </Button>
          <Button size="sm" variant="outline-secondary" disabled={agent.isRefreshing} onClick={() => void agent.refresh()}>
            <RefreshIcon width={15} height={15} />
            {agent.isRefreshing ? t('Refreshing…') : t('Refresh')}
          </Button>
        </div>

        {firstError ? <Alert variant="warning" className="tx-global-alert">{t('Some data failed to refresh')}: {errorText(firstError)}</Alert> : null}
        {restartError ? <Alert variant="danger" className="tx-global-alert"><strong>{t('Restart request failed')}:</strong> {restartError}</Alert> : null}
        {selectedRuntime?.tunnel?.state === 'error' ? (
          <Alert variant="danger" className="tx-global-alert">
            <div className="d-flex flex-wrap align-items-center justify-content-between gap-3">
              <div>
                <strong>{t('Built-in WSS failed to start')}:</strong> {selectedRuntime.tunnel.lastError ?? t('Unknown error')}
                <div className="small mt-1">{t('The local Agent is still running. Correct the Public MCP URL in Settings, save, and restart.')}</div>
              </div>
              <Button size="sm" variant="outline-danger" onClick={() => selectedWorkspace && openWorkspace(selectedWorkspace.id, 'settings')}>{t('Fix settings')}</Button>
            </div>
          </Alert>
        ) : null}

        {view === 'dashboard' && selectedWorkspace ? (
          <section className="tx-page">
            <header className="tx-page-header">
              <p className="tx-page-kicker">{t('Browser management UI')}</p>
              <h2>{t('Dashboard')}</h2>
              <p>{config.workspaces.length} {t('Workspaces')} · {t('Each workspace has independent settings and folders.')}</p>
            </header>
            <div className="tx-page-body">
              <OverviewCards status={status} dashboard={dashboard} config={selectedWorkspace} />
              <div className="mt-4"><DataTables dashboard={dashboard} /></div>
            </div>
          </section>
        ) : null}

        {view === 'quick-setup' && selectedWorkspace ? (
          <QuickSetup
            workspace={selectedWorkspace}
            authorizationPassword={password}
            passwordLoading={agent.password.isLoading}
            passwordError={agent.password.error}
            saving={agent.save.isPending}
            regeneratingPassword={agent.regeneratePassword.isPending}
            onSave={payload => agent.save.mutateAsync({ workspaceId: selectedWorkspace.id, payload })}
            onRegeneratePassword={() => agent.regeneratePassword.mutateAsync(selectedWorkspace.id)}
            onOpenSettings={() => openWorkspace(selectedWorkspace.id, 'settings')}
          />
        ) : null}

        {view === 'workspace' && selectedWorkspace ? (
          <WorkspaceView
            workspace={selectedWorkspace}
            status={status}
            dashboard={dashboard}
            password={password}
            passwordLoading={agent.password.isLoading}
            passwordError={agent.password.error}
            saving={agent.save.isPending}
            regeneratingPassword={agent.regeneratePassword.isPending}
            activeTab={workspaceTab}
            onTabChange={setWorkspaceTab}
            onSave={payload => agent.save.mutateAsync({ workspaceId: selectedWorkspace.id, payload })}
            onRegeneratePassword={() => agent.regeneratePassword.mutateAsync(selectedWorkspace.id)}
            onQuickSetup={() => navigate('quick-setup')}
          />
        ) : null}

        <footer className="app-footer">
          <span>{t('Last updated')}: {formatTime(dashboard.generatedAt)}</span>
          <span>React 管理介面 · {t('Local same-origin assets')}</span>
        </footer>
      </main>
    </div>
  );
}
