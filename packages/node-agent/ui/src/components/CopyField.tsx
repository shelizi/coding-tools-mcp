import { useState } from 'react';
import { useI18n } from '../i18n';
import { CheckIcon, CopyIcon } from './Icons';

interface CopyFieldProps {
  label: string;
  value: string;
  hint?: string;
  secret?: boolean;
}

export function CopyField({ label, value, hint, secret = false }: CopyFieldProps) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const [visible, setVisible] = useState(!secret);

  const copy = async () => {
    if (!value) return;
    await navigator.clipboard.writeText(value);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1_500);
  };

  return (
    <div className="tx-copy-field">
      <div className="tx-copy-label-row">
        <span className="tx-copy-label">{label}</span>
        {secret ? (
          <button type="button" className="tx-link-button" onClick={() => setVisible(current => !current)}>
            {visible ? t('Hide') : t('Show')}
          </button>
        ) : null}
      </div>
      <div className="tx-copy-control">
        <code className="tx-copy-value">{value ? (visible ? value : '••••••••••••••••') : '—'}</code>
        <button type="button" className="tx-copy-button" disabled={!value} onClick={() => void copy()}>
          {copied ? <CheckIcon width={16} height={16} /> : <CopyIcon width={16} height={16} />}
          {copied ? t('Copied') : t('Copy')}
        </button>
      </div>
      {hint ? <p className="tx-copy-hint">{hint}</p> : null}
    </div>
  );
}
