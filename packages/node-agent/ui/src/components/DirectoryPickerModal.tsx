import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import Alert from 'react-bootstrap/Alert';
import Button from 'react-bootstrap/Button';
import Form from 'react-bootstrap/Form';
import InputGroup from 'react-bootstrap/InputGroup';
import ListGroup from 'react-bootstrap/ListGroup';
import Modal from 'react-bootstrap/Modal';
import Spinner from 'react-bootstrap/Spinner';
import Stack from 'react-bootstrap/Stack';
import { browseDirectories } from '../api';
import type { DirectoryBrowsePayload } from '../types';

interface DirectoryPickerModalProps {
  show: boolean;
  workspaceId: string;
  initialPath: string;
  onCancel(): void;
  onSelect(path: string): void;
}

export function DirectoryPickerModal({ show, workspaceId, initialPath, onCancel, onSelect }: DirectoryPickerModalProps) {
  const [payload, setPayload] = useState<DirectoryBrowsePayload | null>(null);
  const [location, setLocation] = useState(initialPath);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const activeRequest = useRef<AbortController | null>(null);

  const loadDirectory = useCallback(async (target?: string) => {
    activeRequest.current?.abort();
    const controller = new AbortController();
    activeRequest.current = controller;
    setLoading(true);
    setError('');
    try {
      const result = await browseDirectories(target?.trim() || undefined, workspaceId, controller.signal);
      setPayload(result);
      setLocation(result.path);
    } catch (reason) {
      if (!controller.signal.aborted) {
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    } finally {
      if (activeRequest.current === controller) {
        activeRequest.current = null;
        setLoading(false);
      }
    }
  }, [workspaceId]);

  useEffect(() => {
    if (!show) {
      activeRequest.current?.abort();
      return;
    }
    setPayload(null);
    setLocation(initialPath);
    void loadDirectory(initialPath);
    return () => activeRequest.current?.abort();
  }, [show, initialPath, loadDirectory]);

  const openLocation = (event: FormEvent) => {
    event.preventDefault();
    void loadDirectory(location);
  };

  return (
    <Modal show={show} onHide={onCancel} size="lg" centered>
      <Modal.Header closeButton>
        <Modal.Title>選擇 Workspace 資料夾</Modal.Title>
      </Modal.Header>
      <Modal.Body>
        <Form onSubmit={openLocation} className="mb-3">
          <Form.Label htmlFor="directory-picker-location">絕對路徑</Form.Label>
          <InputGroup>
            <Form.Control
              id="directory-picker-location"
              className="directory-picker-path"
              value={location}
              onChange={event => setLocation(event.target.value)}
              autoComplete="off"
            />
            <Button type="submit" variant="outline-primary" disabled={loading || !location.trim()}>開啟</Button>
          </InputGroup>
        </Form>

        {payload?.roots.length ? (
          <Stack direction="horizontal" gap={2} className="mb-3 flex-wrap">
            <span className="small text-secondary">根目錄</span>
            {payload.roots.map(root => (
              <Button key={root} type="button" size="sm" variant="outline-secondary" disabled={loading || root === payload.path} onClick={() => void loadDirectory(root)}>
                {root}
              </Button>
            ))}
          </Stack>
        ) : null}

        {error ? <Alert variant="danger">{error}</Alert> : null}
        {payload?.truncated ? <Alert variant="warning">此目錄共有 {payload.totalDirectories} 個子資料夾，目前顯示前 2,000 個。</Alert> : null}

        <div className="directory-picker-toolbar mb-2">
          <Button type="button" size="sm" variant="outline-secondary" disabled={loading || !payload?.parent} onClick={() => payload?.parent && void loadDirectory(payload.parent)}>
            上一層
          </Button>
          <code className="directory-picker-current">{payload?.path ?? location}</code>
        </div>

        <div className="directory-picker-list" aria-live="polite" aria-busy={loading}>
          {loading ? (
            <div className="d-flex justify-content-center align-items-center py-5">
              <Spinner animation="border" role="status"><span className="visually-hidden">載入中</span></Spinner>
            </div>
          ) : payload?.directories.length ? (
            <ListGroup variant="flush">
              {payload.directories.map(directory => (
                <ListGroup.Item key={directory.path} action onClick={() => void loadDirectory(directory.path)}>
                  <span aria-hidden="true">📁</span> {directory.name}
                </ListGroup.Item>
              ))}
            </ListGroup>
          ) : (
            <div className="text-secondary py-4 text-center">此目錄沒有可顯示的子資料夾。</div>
          )}
        </div>
      </Modal.Body>
      <Modal.Footer>
        <Button type="button" variant="secondary" onClick={onCancel}>取消</Button>
        <Button type="button" variant="primary" disabled={loading || !payload} onClick={() => payload && onSelect(payload.path)}>
          選擇這個資料夾
        </Button>
      </Modal.Footer>
    </Modal>
  );
}
