import { useEffect, useMemo, useState } from 'react';
import Alert from 'react-bootstrap/Alert';
import Button from 'react-bootstrap/Button';
import Form from 'react-bootstrap/Form';
import Spinner from 'react-bootstrap/Spinner';
import type {
  ConfigSaveResult,
  ConfigUpdatePayload,
  SecretResult,
  WorkspaceConfigSnapshot
} from '../types';
import { useI18n } from '../i18n';
import { ArrowLeftIcon, CheckIcon, CloudIcon, FolderIcon, PlugIcon, ServerIcon, ShieldIcon } from './Icons';
import { CopyField } from './CopyField';

interface QuickSetupProps {
  workspace: WorkspaceConfigSnapshot;
  authorizationPassword: string;
  passwordLoading: boolean;
  passwordError?: unknown;
  saving: boolean;
  regeneratingPassword: boolean;
  onSave(payload: ConfigUpdatePayload): Promise<ConfigSaveResult>;
  onRegeneratePassword(): Promise<SecretResult>;
  onOpenSettings(): void;
}

function createTunnelClientId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  return Array.from(bytes, value => value.toString(16).padStart(2, '0')).join('');
}

function normalizedEnrollment(value: string): URL {
  let url: URL;
  try {
    url = new URL(value.trim());
  } catch {
    throw new Error('Enter a valid one-time enrollment link.');
  }
  const path = url.pathname.replace(/\/+$/, '');
  if (
    url.protocol !== 'https:' || url.username || url.password || url.search || url.hash
    || !/^\/_tunnel\/enroll\/[A-Za-z0-9]{1,128}$/.test(path)
  ) {
    throw new Error('Use /_tunnel/enroll/<code> for the one-time enrollment link.');
  }
  url.pathname = path;
  return url;
}

const steps = ['Tunnel', 'Project', 'Connection', 'Enable', 'Finish'] as const;

function errorText(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

export function QuickSetup({
  workspace,
  authorizationPassword,
  passwordLoading,
  passwordError,
  saving,
  regeneratingPassword,
  onSave,
  onRegeneratePassword,
  onOpenSettings
}: QuickSetupProps) {
  const { t } = useI18n();
  const [step, setStep] = useState(0);
  const [publicClientId, setPublicClientId] = useState(createTunnelClientId);
  const [enrollmentUrl, setEnrollmentUrl] = useState('');
  const [rotatedPassword, setRotatedPassword] = useState('');
  const [error, setError] = useState('');
  const [saveResult, setSaveResult] = useState<ConfigSaveResult | null>(null);
  const oauthPassword = rotatedPassword || authorizationPassword;

  useEffect(() => {
    setStep(0);
    setEnrollmentUrl('');
    setPublicClientId(createTunnelClientId());
    setRotatedPassword('');
    setSaveResult(null);
    setError('');
  }, [workspace.id]);

  const publicEndpoint = useMemo(() => {
    try {
      const enrollment = normalizedEnrollment(enrollmentUrl);
      return `${enrollment.origin}/builtin/clients/${publicClientId}/mcp`;
    } catch {
      return '';
    }
  }, [enrollmentUrl, publicClientId]);

  const goBack = () => {
    setError('');
    setStep(current => Math.max(0, current - 1));
  };

  const rotatePassword = async () => {
    setError('');
    try {
      const result = await onRegeneratePassword();
      setRotatedPassword(result.value);
    } catch (caught) {
      setError(errorText(caught));
    }
  };

  const submit = async () => {
    setError('');
    try {
      const enrollment = normalizedEnrollment(enrollmentUrl);
      if (!workspace.saved.folders.length) throw new Error(t('Add at least one folder to this workspace.'));
      if (!/^[A-Za-z0-9_-]{1,64}$/.test(publicClientId)) {
        throw new Error(t('Used in the public MCP URL. Keep letters, numbers, underscores, or hyphens.'));
      }
      if (!oauthPassword) throw new Error(t('Authorization password is unavailable. Refresh this page and try again.'));
      const publicUrl = `${enrollment.origin}/builtin/clients/${publicClientId}/mcp`;
      const saved = workspace.saved;
      const result = await onSave({
        name: workspace.name,
        host: saved.host,
        port: saved.port,
        publicBaseUrl: publicUrl.replace(/\/mcp$/, ''),
        dataDir: saved.dataDir,
        securityPolicy: saved.securityPolicy,
        management: { enabled: saved.management.enabled },
        oauth: {
          clientId: saved.oauth.clientId,
          password: '',
          clientSecret: '',
          clearClientSecret: false
        },
        policy: saved.policy,
        folders: saved.folders,
        limits: saved.limits,
        tunnel: {
          enabled: true,
          publicUrl,
          enrollmentUrl: enrollment.toString(),
          clearEnrollmentUrl: false
        }
      });
      setSaveResult(result);
      setStep(4);
    } catch (caught) {
      setError(errorText(caught));
    }
  };

  const reset = () => {
    setStep(0);
    setEnrollmentUrl('');
    setPublicClientId(createTunnelClientId());
    setSaveResult(null);
    setError('');
  };

  const passwordField = passwordLoading ? (
    <div className="tx-secret-loading"><Spinner animation="border" size="sm" />{t('Loading authorization password…')}</div>
  ) : passwordError ? (
    <Alert variant="danger" className="mb-0">{errorText(passwordError)}</Alert>
  ) : (
    <div className="tx-password-preview">
      <CopyField label={t('Authorization password')} value={oauthPassword} hint={t('Enter this after clicking Connect in ChatGPT')} secret />
      <button type="button" className="tx-link-button" disabled={regeneratingPassword} onClick={() => void rotatePassword()}>
        {regeneratingPassword ? t('Generating…') : t('Generate another password')}
      </button>
    </div>
  );

  return (
    <section className="tx-page tx-quick-setup">
      <header className="tx-page-header">
        <p className="tx-page-kicker">{t('Guided setup')}</p>
        <h2>{t('Connect your project to ChatGPT')}</h2>
        <p>{workspace.name} · {workspace.saved.folders.length} {t('Folders')}</p>
        <ol className="tx-progress" aria-label={t('Setup progress')}>
          {steps.map((item, index) => (
            <li key={item} className={index === step ? 'active' : index < step ? 'complete' : ''} aria-current={index === step ? 'step' : undefined}>
              <span>{index < step ? <CheckIcon width={13} height={13} /> : index + 1}</span>
              <strong>{t(item)}</strong>
            </li>
          ))}
        </ol>
      </header>

      <div className="tx-page-body tx-narrow">
        {error ? <Alert variant="danger">{error}</Alert> : null}

        {step === 0 ? (
          <article className="tx-card tx-setup-card">
            <p className="tx-section-label">{t('Reverse proxy')}</p>
            <h3>{t('Choose how this computer gets a public URL')}</h3>
            <p className="tx-muted">{t('This choice determines whether you need an invitation link, frpc, or cloudflared.')}</p>
            <div className="tx-choice-grid three">
              <button type="button" className="tx-choice active" onClick={() => setStep(1)}>
                <ShieldIcon width={24} height={24} />
                <strong>{t('Built-in WSS tunnel (recommended)')}</strong>
                <span>{t('No extra software. Continue with a one-time invitation link from your server administrator.')}</span>
                <small>{t('Available now')}</small>
              </button>
              <div className="tx-choice disabled" aria-disabled="true">
                <ServerIcon width={24} height={24} />
                <strong>{t('FRP')}</strong>
                <span>{t('For a self-hosted or company FRP server. The wizard can install frpc and save the server profile.')}</span>
                <small>{t('Not available')}</small>
              </div>
              <div className="tx-choice disabled" aria-disabled="true">
                <CloudIcon width={24} height={24} />
                <strong>{t('Cloudflare')}</strong>
                <span>{t('Use a temporary Quick Tunnel or a stable Named Tunnel. The wizard can install cloudflared.')}</span>
                <small>{t('Not available')}</small>
              </div>
            </div>
          </article>
        ) : null}

        {step === 1 ? (
          <article className="tx-card tx-setup-card">
            <div className="tx-card-heading">
              <div>
                <FolderIcon width={24} height={24} />
                <h3>{workspace.name}</h3>
                <p className="tx-muted">{t('This workspace has its own settings, authorization password, and folders.')}</p>
              </div>
              <button type="button" className="tx-btn-ghost" onClick={goBack}><ArrowLeftIcon width={16} height={16} />{t('Back')}</button>
            </div>
            <div className="tx-folder-list mt-4">
              {workspace.saved.folders.map(folder => (
                <div key={folder.id}>
                  <FolderIcon width={17} height={17} />
                  <span><strong>{folder.name}</strong><code>{folder.path}</code><small>{folder.id}</small></span>
                </div>
              ))}
            </div>
            <div className="tx-actions-row">
              <Button type="button" disabled={!workspace.saved.folders.length} onClick={() => setStep(2)}>{t('Continue with this workspace')}</Button>
              <Button type="button" variant="outline-secondary" onClick={onOpenSettings}>{t('Open settings')}</Button>
            </div>
          </article>
        ) : null}

        {step === 2 ? (
          <article className="tx-card tx-setup-card">
            <div className="tx-card-heading">
              <div>
                <p className="tx-section-label">{t('Connection method')}</p>
                <h3>{t('How do you want to connect ChatGPT?')}</h3>
                <p className="tx-muted">{t('Node Agent currently exposes MCP only.')}</p>
              </div>
              <button type="button" className="tx-btn-ghost" onClick={goBack}><ArrowLeftIcon width={16} height={16} />{t('Back')}</button>
            </div>
            <div className="tx-choice-grid two">
              <button type="button" className="tx-choice active" onClick={() => setStep(3)}>
                <PlugIcon width={24} height={24} />
                <strong>{t('MCP Connector')}</strong>
                <span>{t('Recommended when your ChatGPT plan supports custom MCP connectors. Uses the public MCP endpoint and OAuth.')}</span>
                <small>{t('Available now')}</small>
              </button>
              <div className="tx-choice disabled" aria-disabled="true">
                <CloudIcon width={24} height={24} />
                <strong>{t('GPT Actions')}</strong>
                <span>{t('Use this when building a custom GPT. Import the OpenAPI schema and configure a Bearer key.')}</span>
                <small>{t('Not available')}</small>
              </div>
            </div>
          </article>
        ) : null}

        {step === 3 ? (
          <article className="tx-card tx-setup-card">
            <div className="tx-card-heading">
              <div>
                <p className="tx-section-label">{t('Built-in WSS tunnel')}</p>
                <h3>{t('Prepare and enable {service}', { service: t('MCP') })}</h3>
                <p className="tx-muted">{t('The wizard validates and securely saves every required value. Restart the Agent to register and start the tunnel.')}</p>
              </div>
              <button type="button" className="tx-btn-ghost" disabled={saving} onClick={goBack}><ArrowLeftIcon width={16} height={16} />{t('Back')}</button>
            </div>

            <Form.Group className="mt-4" controlId="quick-enrollment-url">
              <Form.Label>{t('One-time enrollment link')}</Form.Label>
              <Form.Control type="password" autoComplete="off" placeholder="https://example.com/_tunnel/enroll/abc123" value={enrollmentUrl} disabled={saving} onChange={event => setEnrollmentUrl(event.target.value)} />
              <Form.Text>{t('Expected format: https://server/_tunnel/enroll/code')}</Form.Text>
            </Form.Group>

            <details className="tx-advanced mt-4">
              <summary>{t('Public connection ID')}</summary>
              <Form.Group className="mt-3" controlId="quick-public-client-id">
                <Form.Control value={publicClientId} readOnly />
                <Form.Text>{t('Generated as a random UUID for registration. The server-assigned Client ID becomes authoritative after enrollment.')}</Form.Text>
                <div className="mt-2"><Button type="button" size="sm" variant="outline-secondary" disabled={saving} onClick={() => setPublicClientId(createTunnelClientId())}>{t('Generate another connection ID')}</Button></div>
              </Form.Group>
            </details>

            <div className="mt-4">{passwordField}</div>
            {publicEndpoint ? <div className="mt-4"><CopyField label={t('Provisional MCP endpoint')} value={publicEndpoint} hint={t('Enrollment replaces this provisional ID when the server assigns a different Client ID.')} /></div> : null}

            <Button className="mt-4" type="button" disabled={saving || !enrollmentUrl.trim() || !oauthPassword} onClick={() => void submit()}>
              {saving ? t('Saving…') : t('Save quick setup')}
            </Button>
          </article>
        ) : null}

        {step === 4 && saveResult ? (
          <article className="tx-card tx-setup-card tx-complete-card">
            <div className="tx-complete-icon"><CheckIcon width={28} height={28} /></div>
            <p className="tx-page-kicker">{saveResult.restartRequired ? t('Quick setup saved') : t('Service enabled')}</p>
            <h3>{saveResult.restartRequired ? t('Restart the Agent, then finish setup in ChatGPT') : t('Now finish the setup in ChatGPT')}</h3>
            <p className="tx-muted">{workspace.name}</p>

            <Alert variant={saveResult.restartRequired ? 'warning' : 'success'} className="mt-4">
              <strong>{saveResult.restartRequired ? t('Restart required') : t('Running')}</strong>
              <div>{saveResult.restartRequired ? t('Restart the Agent to apply the saved tunnel and OAuth settings.') : t('Service enabled')}</div>
            </Alert>

            <div className="tx-complete-grid">
              <article className="tx-inset-card">
                <p className="tx-section-label">{t('ChatGPT steps')}</p>
                <ol className="tx-step-list">
                  <li><strong>1.</strong>{t('Open ChatGPT Settings → Connectors, then create a custom MCP connector.')}</li>
                  <li><strong>2.</strong>{t('After restart, copy the final Public MCP endpoint from the workspace overview and choose OAuth authentication.')}</li>
                  <li><strong>3.</strong>{t('Enter the OAuth Client ID and leave Client Secret empty.')}</li>
                  <li><strong>4.</strong>{t('Select Next, click Connect, then enter the authorization password.')}</li>
                </ol>
              </article>
              <article className="tx-inset-card">
                <p className="tx-section-label">{t('GPT configuration')}</p>
                <div className="tx-copy-stack">
                  <CopyField
                    label={t('Public MCP endpoint')}
                    value={saveResult.saved.tunnel.publicUrl || publicEndpoint}
                    hint={saveResult.restartRequired ? t('Enrollment replaces this provisional ID when the server assigns a different Client ID.') : undefined}
                  />
                  {saveResult.restartRequired ? <Alert variant="info" className="mb-0">{t('The final Public MCP endpoint is available on the workspace overview after enrollment completes.')}</Alert> : null}
                  <CopyField label="OAuth Client ID" value={saveResult.saved.oauth.clientId} />
                  <CopyField label={t('Authorization password')} value={oauthPassword} secret />
                </div>
              </article>
            </div>

            <Alert variant="info" className="mt-4 mb-0">{t('The authorization password remains available from this workspace overview at any time.')}</Alert>
            <div className="tx-actions-row">
              <Button type="button" onClick={reset}>{t('Start another setup')}</Button>
              <Button type="button" variant="outline-secondary" onClick={onOpenSettings}>{t('Open settings')}</Button>
            </div>
          </article>
        ) : null}
      </div>
    </section>
  );
}
