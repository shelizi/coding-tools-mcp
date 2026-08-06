import { useEffect, useState, type FormEvent } from 'react';
import Accordion from 'react-bootstrap/Accordion';
import Alert from 'react-bootstrap/Alert';
import Badge from 'react-bootstrap/Badge';
import Button from 'react-bootstrap/Button';
import Card from 'react-bootstrap/Card';
import Col from 'react-bootstrap/Col';
import Form from 'react-bootstrap/Form';
import Row from 'react-bootstrap/Row';
import Stack from 'react-bootstrap/Stack';
import type {
  ConfigSaveResult,
  WorkspaceConfigSnapshot,
  ConfigUpdatePayload,
  PermissionMode,
  ToolProfileSetting,
  WorkspaceFolder
} from '../types';

interface ConfigFormProps {
  snapshot: WorkspaceConfigSnapshot;
  saving: boolean;
  onSave(payload: ConfigUpdatePayload): Promise<ConfigSaveResult>;
}

interface FormState {
  workspaceName: string;
  host: string;
  port: string;
  publicBaseUrl: string;
  dataDir: string;
  permissionMode: PermissionMode;
  toolProfile: ToolProfileSetting;
  managementEnabled: boolean;
  oauthClientId: string;
  oauthPassword: string;
  oauthClientSecret: string;
  clearClientSecret: boolean;
  allowedCommands: string;
  workspaceLocalEntries: boolean;
  workspaceScriptExtensions: string;
  maxPatchBytes: string;
  folders: WorkspaceFolder[];
  blockingConcurrency: string;
  processConcurrency: string;
  globalBlockingConcurrency: string;
  globalProcessConcurrency: string;
  activeSessionLimit: string;
  maxOutputBytes: string;
  tunnelEnabled: boolean;
  tunnelPublicUrl: string;
  tunnelEnrollmentUrl: string;
  clearEnrollmentUrl: boolean;
}

interface FormMessage {
  variant: 'success' | 'warning' | 'danger';
  text: string;
}

function createState(snapshot: WorkspaceConfigSnapshot): FormState {
  const config = snapshot.saved;
  return {
    workspaceName: snapshot.name,
    host: config.host,
    port: String(config.port),
    publicBaseUrl: config.publicBaseUrl,
    dataDir: config.dataDir,
    permissionMode: config.permissionMode,
    toolProfile: config.toolProfile,
    managementEnabled: config.management.enabled,
    oauthClientId: config.oauth.clientId,
    oauthPassword: '',
    oauthClientSecret: '',
    clearClientSecret: false,
    allowedCommands: config.policy.allowedCommands.join('\n'),
    workspaceLocalEntries: config.policy.workspaceLocalEntries,
    workspaceScriptExtensions: config.policy.workspaceScriptExtensions.join('\n'),
    maxPatchBytes: String(config.policy.maxPatchBytes),
    folders: config.folders.map(folder => ({ ...folder })),
    blockingConcurrency: String(config.limits.blockingConcurrency),
    processConcurrency: String(config.limits.processConcurrency),
    globalBlockingConcurrency: String(config.limits.globalBlockingConcurrency),
    globalProcessConcurrency: String(config.limits.globalProcessConcurrency),
    activeSessionLimit: String(config.limits.activeSessionLimit),
    maxOutputBytes: String(config.limits.maxOutputBytes),
    tunnelEnabled: config.tunnel.enabled,
    tunnelPublicUrl: config.tunnel.publicUrl,
    tunnelEnrollmentUrl: '',
    clearEnrollmentUrl: false
  };
}

function integer(value: string, name: string, minimum: number, maximum: number): number {
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${name} 必須是 ${minimum} 到 ${maximum} 之間的整數。`);
  }
  return parsed;
}

function stringList(value: string, name: string): string[] {
  const entries = [...new Set(value.split(/[\r\n,]+/).map(item => item.trim()).filter(Boolean))];
  if (!entries.length) throw new Error(`${name} 至少需要一個項目。`);
  return entries;
}

function normalizeTunnelPublicUrl(value: string): string {
  let url: URL;
  try {
    url = new URL(value.trim());
  } catch {
    throw new Error('Public MCP URL 必須是完整網址。');
  }
  if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash) {
    throw new Error('Public MCP URL 必須是乾淨的 HTTPS 網址，不可包含帳密、query 或 fragment。');
  }
  if (!/^\/builtin\/clients\/[A-Za-z0-9_-]{1,64}\/mcp\/?$/.test(url.pathname)) {
    throw new Error('Public MCP URL 路徑必須是 /builtin/clients/<client-id>/mcp。');
  }
  url.pathname = url.pathname.replace(/\/$/, '');
  return url.toString().replace(/\/$/, '');
}

function payload(state: FormState): ConfigUpdatePayload {
  if (!state.workspaceName.trim()) throw new Error('Workspace 名稱不可空白。');
  if (!state.host.trim()) throw new Error('Bind Host 不可空白。');
  if (!state.dataDir.trim()) throw new Error('Data Directory 不可空白。');
  if (!state.oauthClientId.trim()) throw new Error('OAuth Client ID 不可空白。');
  if (!state.folders.length) throw new Error('每個 Workspace 至少需要一個資料夾。');
  const folders = state.folders.map((folder, index) => {
    const folderPath = folder.path.trim();
    if (!folderPath) throw new Error(`資料夾 ${index + 1} 的路徑不可空白。`);
    const id = folder.id.trim();
    const name = folder.name.trim();
    return id && name ? { id, name, path: folderPath } : { path: folderPath };
  });
  if (state.tunnelEnabled && !state.tunnelPublicUrl.trim()) {
    throw new Error('啟用 Built-in WSS 時必須提供 Public MCP URL。');
  }
  const tunnelPublicUrl = state.tunnelPublicUrl.trim()
    ? normalizeTunnelPublicUrl(state.tunnelPublicUrl)
    : '';

  return {
    host: state.host.trim(),
    name: state.workspaceName.trim(),
    port: integer(state.port, 'Port', 1, 65_535),
    publicBaseUrl: state.publicBaseUrl.trim(),
    dataDir: state.dataDir.trim(),
    permissionMode: state.permissionMode,
    toolProfile: state.toolProfile,
    management: { enabled: state.managementEnabled },
    oauth: {
      clientId: state.oauthClientId.trim(),
      password: state.oauthPassword,
      clientSecret: state.oauthClientSecret,
      clearClientSecret: state.clearClientSecret
    },
    policy: {
      allowedCommands: stringList(state.allowedCommands, '允許命令'),
      workspaceLocalEntries: state.workspaceLocalEntries,
      workspaceScriptExtensions: stringList(state.workspaceScriptExtensions, 'Workspace script extensions'),
      maxPatchBytes: integer(state.maxPatchBytes, 'Patch bytes 上限', 1, 16 * 1024 * 1024)
    },
    folders,
    limits: {
      blockingConcurrency: integer(state.blockingConcurrency, '檔案/Git 併發', 1, 256),
      processConcurrency: integer(state.processConcurrency, '程序併發', 1, 128),
      globalBlockingConcurrency: integer(state.globalBlockingConcurrency, '全域檔案/Git 併發', 1, 65_535),
      globalProcessConcurrency: integer(state.globalProcessConcurrency, '全域程序併發', 1, 65_535),
      activeSessionLimit: integer(state.activeSessionLimit, 'Session 上限', 1, 65_535),
      maxOutputBytes: integer(state.maxOutputBytes, '每串流保留 bytes', 1_024, 16 * 1024 * 1024)
    },
    tunnel: {
      enabled: state.tunnelEnabled,
      publicUrl: tunnelPublicUrl,
      enrollmentUrl: state.tunnelEnrollmentUrl,
      clearEnrollmentUrl: state.clearEnrollmentUrl
    }
  };
}

export function ConfigForm({ snapshot, saving, onSave }: ConfigFormProps) {
  const [state, setState] = useState(() => createState(snapshot));
  const [message, setMessage] = useState<FormMessage | null>(null);

  useEffect(() => {
    setState(createState(snapshot));
  }, [snapshot]);

  const updateFolder = (index: number, key: keyof WorkspaceFolder, value: string) => {
    setState(current => ({
      ...current,
      folders: current.folders.map((folder, folderIndex) => folderIndex === index ? { ...folder, [key]: value } : folder)
    }));
  };

  const addFolder = () => {
    setState(current => ({
      ...current,
      folders: [...current.folders, { id: '', name: '', path: '' }]
    }));
  };

  const removeFolder = (index: number) => {
    setState(current => ({ ...current, folders: current.folders.filter((_, folderIndex) => folderIndex !== index) }));
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setMessage(null);
    try {
      const result = await onSave(payload(state));
      setState(current => ({
        ...current,
        oauthPassword: '',
        oauthClientSecret: '',
        tunnelEnrollmentUrl: '',
        clearClientSecret: false,
        clearEnrollmentUrl: false
      }));
      setMessage({
        variant: result.restartRequired ? 'warning' : 'success',
        text: result.restartRequired ? '設定已儲存，請重新啟動 Agent 套用。' : '設定已儲存，目前不需要重新啟動。'
      });
    } catch (error) {
      setMessage({ variant: 'danger', text: error instanceof Error ? error.message : String(error) });
    }
  };

  return (
    <Form onSubmit={submit}>
      {snapshot.environmentOverrides.length ? (
        <Alert variant="info">
          下列環境變數會優先於設定檔：<code>{snapshot.environmentOverrides.join(', ')}</code>
        </Alert>
      ) : (
        <Alert variant="secondary">目前沒有環境變數覆寫。</Alert>
      )}

      <Accordion alwaysOpen defaultActiveKey={['server', 'workspaces']} className="settings-accordion">
        <Accordion.Item eventKey="identity">
          <Accordion.Header>Workspace</Accordion.Header>
          <Accordion.Body>
            <Form.Group controlId={`workspace-name-${snapshot.id}`}>
              <Form.Label>Workspace 名稱</Form.Label>
              <Form.Control value={state.workspaceName} onChange={event => setState(current => ({ ...current, workspaceName: event.target.value }))} />
              <Form.Text className="text-secondary">這是側欄顯示名稱；此 Workspace 的連線、權限、密碼與資料夾皆獨立儲存。</Form.Text>
            </Form.Group>
          </Accordion.Body>
        </Accordion.Item>

        <Accordion.Item eventKey="server">
          <Accordion.Header>伺服器與權限</Accordion.Header>
          <Accordion.Body>
            <Row className="g-3">
              <Col md={6} xl={4}>
                <Form.Group controlId="host">
                  <Form.Label>Bind Host</Form.Label>
                  <Form.Control value={state.host} onChange={event => setState(current => ({ ...current, host: event.target.value }))} />
                </Form.Group>
              </Col>
              <Col md={6} xl={4}>
                <Form.Group controlId="port">
                  <Form.Label>Port</Form.Label>
                  <Form.Control type="number" min={1} max={65_535} value={state.port} onChange={event => setState(current => ({ ...current, port: event.target.value }))} />
                </Form.Group>
              </Col>
              <Col md={6} xl={4}>
                <Form.Group controlId="permissionMode">
                  <Form.Label>權限模式</Form.Label>
                  <Form.Select value={state.permissionMode} onChange={event => setState(current => ({ ...current, permissionMode: event.target.value as PermissionMode }))}>
                    <option value="read-only">read-only</option>
                    <option value="guarded">guarded</option>
                    <option value="trusted">trusted</option>
                    <option value="dangerous">dangerous</option>
                  </Form.Select>
                </Form.Group>
              </Col>
              <Col md={6} xl={4}>
                <Form.Group controlId="toolProfile">
                  <Form.Label>工具 Profile</Form.Label>
                  <Form.Select value={state.toolProfile} onChange={event => setState(current => ({ ...current, toolProfile: event.target.value as ToolProfileSetting }))}>
                    <option value="core">core（依權限自動）</option>
                    <option value="trusted-core">trusted-core</option>
                    <option value="guarded-core">guarded-core</option>
                    <option value="read-only">read-only</option>
                    <option value="advanced">advanced</option>
                    <option value="compat-readonly-all">compat-readonly-all</option>
                  </Form.Select>
                  <Form.Text className="text-secondary">
                    儲存值：{snapshot.saved.toolProfile} · 生效值：{snapshot.effective.activeToolProfile}
                  </Form.Text>
                </Form.Group>
              </Col>
              <Col xs={12}>
                <Form.Group controlId="publicBaseUrl">
                  <Form.Label>Public Base URL</Form.Label>
                  <Form.Control value={state.publicBaseUrl} placeholder="https://example.com/builtin/clients/..." onChange={event => setState(current => ({ ...current, publicBaseUrl: event.target.value }))} />
                </Form.Group>
              </Col>
              <Col xs={12}>
                <Form.Group controlId="dataDir">
                  <Form.Label>Data Directory</Form.Label>
                  <Form.Control value={state.dataDir} onChange={event => setState(current => ({ ...current, dataDir: event.target.value }))} />
                </Form.Group>
              </Col>
              <Col xs={12}>
                <Form.Check type="switch" id="managementEnabled" label="啟用瀏覽器管理介面" checked={state.managementEnabled} onChange={event => setState(current => ({ ...current, managementEnabled: event.target.checked }))} />
              </Col>
            </Row>
          </Accordion.Body>
        </Accordion.Item>

        <Accordion.Item eventKey="policy">
          <Accordion.Header>執行政策</Accordion.Header>
          <Accordion.Body>
            <Row className="g-3">
              <Col md={6}>
                <Form.Group controlId="allowedCommands">
                  <Form.Label>允許命令</Form.Label>
                  <Form.Control as="textarea" rows={7} value={state.allowedCommands} onChange={event => setState(current => ({ ...current, allowedCommands: event.target.value }))} />
                  <Form.Text className="text-secondary">每行一個可執行命令；與 Rust policy editor 的 allowed commands 同步。</Form.Text>
                </Form.Group>
              </Col>
              <Col md={6}>
                <Form.Group controlId="workspaceScriptExtensions">
                  <Form.Label>Workspace script extensions</Form.Label>
                  <Form.Control as="textarea" rows={7} value={state.workspaceScriptExtensions} onChange={event => setState(current => ({ ...current, workspaceScriptExtensions: event.target.value }))} />
                  <Form.Text className="text-secondary">每行一個副檔名，例如 .ps1、.cmd、.sh。</Form.Text>
                </Form.Group>
              </Col>
              <Col md={6}>
                <Form.Group controlId="maxPatchBytes">
                  <Form.Label>Patch bytes 上限</Form.Label>
                  <Form.Control type="number" min={1} max={16 * 1024 * 1024} value={state.maxPatchBytes} onChange={event => setState(current => ({ ...current, maxPatchBytes: event.target.value }))} />
                </Form.Group>
              </Col>
              <Col md={6} className="d-flex align-items-end">
                <Form.Check type="switch" id="workspaceLocalEntries" label="允許 Workspace-local executables" checked={state.workspaceLocalEntries} onChange={event => setState(current => ({ ...current, workspaceLocalEntries: event.target.checked }))} />
              </Col>
            </Row>
          </Accordion.Body>
        </Accordion.Item>

        <Accordion.Item eventKey="oauth">
          <Accordion.Header>OAuth 與加密秘密</Accordion.Header>
          <Accordion.Body>
            <Stack direction="horizontal" gap={2} className="mb-3 flex-wrap">
              <Badge bg={snapshot.saved.oauth.passwordConfigured ? 'success' : 'secondary'}>密碼 {snapshot.saved.oauth.passwordConfigured ? '已設定' : '使用預設值'}</Badge>
              <Badge bg={snapshot.saved.oauth.clientSecretConfigured ? 'success' : 'secondary'}>Client Secret {snapshot.saved.oauth.clientSecretConfigured ? '已設定' : '未設定'}</Badge>
              <Badge bg="secondary">Token Secret {snapshot.saved.oauth.tokenSecretSource}</Badge>
            </Stack>
            <Row className="g-3">
              <Col xs={12}>
                <Form.Group controlId="oauthClientId">
                  <Form.Label>Client ID</Form.Label>
                  <Form.Control value={state.oauthClientId} onChange={event => setState(current => ({ ...current, oauthClientId: event.target.value }))} />
                </Form.Group>
              </Col>
              <Col md={6}>
                <Form.Group controlId="oauthPassword">
                  <Form.Label>替換登入密碼</Form.Label>
                  <Form.Control type="password" autoComplete="new-password" value={state.oauthPassword} placeholder="留空保留原設定" onChange={event => setState(current => ({ ...current, oauthPassword: event.target.value }))} />
                </Form.Group>
              </Col>
              <Col md={6}>
                <Form.Group controlId="oauthClientSecret">
                  <Form.Label>替換 Client Secret</Form.Label>
                  <Form.Control type="password" autoComplete="new-password" value={state.oauthClientSecret} placeholder="留空保留原設定" onChange={event => setState(current => ({ ...current, oauthClientSecret: event.target.value }))} />
                </Form.Group>
              </Col>
              <Col xs={12}>
                <Form.Check type="switch" id="clearClientSecret" label="清除既有 Client Secret" checked={state.clearClientSecret} onChange={event => setState(current => ({ ...current, clearClientSecret: event.target.checked }))} />
              </Col>
            </Row>
          </Accordion.Body>
        </Accordion.Item>

        <Accordion.Item eventKey="workspaces">
          <Accordion.Header>Workspace 資料夾</Accordion.Header>
          <Accordion.Body>
            <Stack gap={3}>
              {state.folders.map((folder, index) => (
                <Card key={index} className="workspace-editor">
                  <Card.Body>
                    <Row className="g-3 align-items-end">
                      <Col md={11}>
                        <Form.Group controlId={`folder-path-${index}`}>
                          <Form.Label>絕對路徑</Form.Label>
                          <Form.Control value={folder.path} onChange={event => updateFolder(index, 'path', event.target.value)} />
                        </Form.Group>
                      </Col>
                      <Col md={1} className="d-grid">
                        <Button type="button" variant="outline-danger" disabled={state.folders.length === 1} onClick={() => removeFolder(index)} aria-label={`刪除資料夾 ${index + 1}`}>×</Button>
                      </Col>
                    </Row>
                  </Card.Body>
                </Card>
              ))}
              <div><Button type="button" variant="outline-primary" onClick={addFolder}>新增資料夾</Button></div>
            </Stack>
          </Accordion.Body>
        </Accordion.Item>

        <Accordion.Item eventKey="limits">
          <Accordion.Header>資源限制</Accordion.Header>
          <Accordion.Body>
            <Row className="g-3">
              <Col md={6} xl={4}><Form.Group controlId="blockingConcurrency"><Form.Label>Workspace 檔案/Git 併發</Form.Label><Form.Control type="number" min={1} max={256} value={state.blockingConcurrency} onChange={event => setState(current => ({ ...current, blockingConcurrency: event.target.value }))} /></Form.Group></Col>
              <Col md={6} xl={4}><Form.Group controlId="processConcurrency"><Form.Label>Workspace 程序併發</Form.Label><Form.Control type="number" min={1} max={128} value={state.processConcurrency} onChange={event => setState(current => ({ ...current, processConcurrency: event.target.value }))} /></Form.Group></Col>
              <Col md={6} xl={4}><Form.Group controlId="globalBlockingConcurrency"><Form.Label>全域檔案/Git 併發</Form.Label><Form.Control type="number" min={1} max={65_535} value={state.globalBlockingConcurrency} onChange={event => setState(current => ({ ...current, globalBlockingConcurrency: event.target.value }))} /></Form.Group></Col>
              <Col md={6} xl={4}><Form.Group controlId="globalProcessConcurrency"><Form.Label>全域程序併發</Form.Label><Form.Control type="number" min={1} max={65_535} value={state.globalProcessConcurrency} onChange={event => setState(current => ({ ...current, globalProcessConcurrency: event.target.value }))} /></Form.Group></Col>
              <Col md={6} xl={4}><Form.Group controlId="activeSessionLimit"><Form.Label>Session 上限</Form.Label><Form.Control type="number" min={1} max={65_535} value={state.activeSessionLimit} onChange={event => setState(current => ({ ...current, activeSessionLimit: event.target.value }))} /></Form.Group></Col>
              <Col md={6} xl={4}><Form.Group controlId="maxOutputBytes"><Form.Label>每串流保留 bytes</Form.Label><Form.Control type="number" min={1_024} max={16 * 1024 * 1024} value={state.maxOutputBytes} onChange={event => setState(current => ({ ...current, maxOutputBytes: event.target.value }))} /></Form.Group></Col>
            </Row>
          </Accordion.Body>
        </Accordion.Item>

        <Accordion.Item eventKey="tunnel">
          <Accordion.Header>Built-in WSS</Accordion.Header>
          <Accordion.Body>
            <Row className="g-3">
              <Col xs={12}><Form.Check type="switch" id="tunnelEnabled" label="啟用內建 tunnel" checked={state.tunnelEnabled} onChange={event => setState(current => ({ ...current, tunnelEnabled: event.target.checked }))} /></Col>
              <Col xs={12}>
                <Form.Group controlId="tunnelPublicUrl">
                  <Form.Label>Public MCP URL</Form.Label>
                  <Form.Control
                    value={state.tunnelPublicUrl}
                    placeholder="https://server.example/builtin/clients/client-id/mcp"
                    onChange={event => setState(current => ({ ...current, tunnelPublicUrl: event.target.value }))}
                  />
                  <Form.Text className="text-secondary">必須使用 /builtin/clients/&lt;client-id&gt;/mcp 路徑。</Form.Text>
                </Form.Group>
              </Col>
              <Col xs={12}><Form.Group controlId="tunnelEnrollmentUrl"><Form.Label>替換 Enrollment URL</Form.Label><Form.Control type="password" autoComplete="new-password" value={state.tunnelEnrollmentUrl} placeholder="留空保留原設定" onChange={event => setState(current => ({ ...current, tunnelEnrollmentUrl: event.target.value }))} /></Form.Group></Col>
              <Col xs={12}><Form.Check type="switch" id="clearEnrollmentUrl" label="清除既有 Enrollment URL" checked={state.clearEnrollmentUrl} onChange={event => setState(current => ({ ...current, clearEnrollmentUrl: event.target.checked }))} /></Col>
            </Row>
          </Accordion.Body>
        </Accordion.Item>
      </Accordion>

      <div className="settings-actions mt-4">
        <Stack direction="horizontal" gap={3} className="flex-wrap">
          <Button type="submit" disabled={saving}>{saving ? '儲存中…' : '儲存設定'}</Button>
          <span className="small text-secondary">儲存採用加密 secrets + 公開設定 rollback，執行中的 Agent 不會被熱修改。</span>
        </Stack>
        {message ? <Alert variant={message.variant} className="mt-3 mb-0">{message.text}</Alert> : null}
      </div>
    </Form>
  );
}
