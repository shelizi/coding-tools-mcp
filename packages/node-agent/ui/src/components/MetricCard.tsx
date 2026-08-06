import Card from 'react-bootstrap/Card';

interface MetricCardProps {
  label: string;
  value: string | number;
  detail?: string;
  tone?: 'default' | 'success' | 'warning' | 'danger';
}

export function MetricCard({ label, value, detail, tone = 'default' }: MetricCardProps) {
  return (
    <Card className={`metric-card h-100 metric-${tone}`}>
      <Card.Body>
        <div className="metric-label">{label}</div>
        <div className="metric-value">{value}</div>
        {detail ? <div className="metric-detail">{detail}</div> : null}
      </Card.Body>
    </Card>
  );
}
