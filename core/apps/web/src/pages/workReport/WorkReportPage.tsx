import { useCallback, useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import type { WorkspaceWorkReport } from "@ctx/types";
import { getWorkspaceWorkReport } from "../../api/clientWorkspaces";
import { WorkReportView } from "./WorkReportView";

export default function WorkReportPage() {
  const { id, workId } = useParams<{ id: string; workId: string }>();
  const [report, setReport] = useState<WorkspaceWorkReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadReport = useCallback(async () => {
    if (!id || !workId) {
      setError("Missing Work route parameters.");
      setLoading(false);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const next = await getWorkspaceWorkReport(id, workId);
      setReport(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load Work report.");
    } finally {
      setLoading(false);
    }
  }, [id, workId]);

  useEffect(() => {
    void loadReport();
  }, [loadReport]);

  if (!id || !workId) {
    return (
      <main className="work-report-page" role="alert">
        Missing Work route parameters.
      </main>
    );
  }
  if (loading && !report) {
    return (
      <main className="work-report-page work-report-loading" aria-busy="true" role="status">
        Loading Work report...
      </main>
    );
  }
  if (error && !report) {
    return (
      <main className="work-report-page work-report-error" role="alert">
        <h1>Work report unavailable</h1>
        <p>{error}</p>
        <button className="work-report-refresh" type="button" onClick={loadReport}>
          Retry
        </button>
        <Link to={`/workspaces/${encodeURIComponent(id)}`}>Back to workspace</Link>
      </main>
    );
  }
  if (!report) {
    return (
      <main className="work-report-page" role="status">
        No Work report is available.
      </main>
    );
  }
  return <WorkReportView report={report} onRefresh={loadReport} />;
}
