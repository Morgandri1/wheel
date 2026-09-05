"use client";

import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { Button, Field, Input, Select, Textarea } from "@/components/ui";
import { toast, toastError } from "@/components/ui/toast";
import { validateColumnName } from "@/lib/validate";
import { COLUMN_TYPES } from "@/lib/schema";
import type { EngineApi, QueryResult } from "@/lib/api";
import type { Column, ColumnType, TableNode } from "@/lib/schema";

const PAGE = 25;

export function TablePanel({
  node,
  api,
  projectId,
  onChanged,
}: {
  node: TableNode;
  api: EngineApi;
  projectId: string;
  onChanged: () => void;
}) {
  const [columns, setColumns] = useState<Column[]>(node.config.columns);
  const [saving, setSaving] = useState(false);
  const [tab, setTab] = useState<"rows" | "columns" | "query">("rows");
  const [offset, setOffset] = useState(0);
  const [sql, setSql] = useState(`SELECT * FROM t_${node.name} LIMIT 20;`);
  const [queryResult, setQueryResult] = useState<QueryResult | null>(null);
  const [queryError, setQueryError] = useState<string | null>(null);

  useEffect(() => {
    setColumns(node.config.columns);
    setOffset(0);
    setQueryResult(null);
    setQueryError(null);
    setSql(`SELECT * FROM t_${node.name} LIMIT 20;`);
  }, [node.id, node.name, node.config.columns]);

  const rows = useQuery({
    queryKey: ["table-rows", projectId, node.id, offset],
    queryFn: () => api.table(node.id).rows(PAGE, offset),
    enabled: tab === "rows",
  });

  const columnErrors = columns.map((c) => validateColumnName(c.name));
  const dirty = JSON.stringify(columns) !== JSON.stringify(node.config.columns);

  const save = async () => {
    setSaving(true);
    try {
      await api.patchNode(node.id, { config: { columns } });
      onChanged();
      toast("Columns saved.");
    } catch (e) {
      toastError(e, "Couldn't change those columns.");
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <p className="text-meta text-ink-dim">
        A sqlite table called <span className="ident">t_{node.name}</span>. Agents with a read wire
        can query it; a write wire also lets them change it.
      </p>

      <div className="flex items-center gap-px border border-rule">
        {(["rows", "columns", "query"] as const).map((t) => (
          <button
            key={t}
            data-testid={`table-tab-${t}`}
            onClick={() => setTab(t)}
            className={`flex-1 px-2 py-1 text-micro capitalize transition-colors ${
              tab === t ? "bg-[var(--panel-2)] text-ink" : "text-ink-dim hover:text-ink"
            }`}
          >
            {t}
          </button>
        ))}
      </div>

      {tab === "columns" ? (
        <div className="flex flex-col gap-2" data-testid="inspector-table-columns">
          {columns.map((col, i) => (
            <div key={i} className="flex items-start gap-1.5">
              <div className="flex-1">
                <Input
                  mono
                  aria-label={`Column ${i + 1} name`}
                  data-testid={`table-column-name-${i}`}
                  value={col.name}
                  onChange={(e) =>
                    setColumns(columns.map((c, j) => (i === j ? { ...c, name: e.target.value } : c)))
                  }
                />
                {columnErrors[i] ? (
                  <p className="mt-1 text-micro text-[var(--danger)]">{columnErrors[i]}</p>
                ) : null}
              </div>
              <Select
                aria-label={`Column ${i + 1} type`}
                data-testid={`table-column-type-${i}`}
                className="w-[92px]"
                value={col.type}
                onChange={(e) =>
                  setColumns(
                    columns.map((c, j) => (i === j ? { ...c, type: e.target.value as ColumnType } : c)),
                  )
                }
              >
                {COLUMN_TYPES.map((t) => (
                  <option key={t} value={t}>
                    {t}
                  </option>
                ))}
              </Select>
              <Button
                size="sm"
                tone="ghost"
                aria-label={`Remove column ${col.name}`}
                data-testid={`btn-remove-column-${i}`}
                onClick={() => setColumns(columns.filter((_, j) => j !== i))}
              >
                ×
              </Button>
            </div>
          ))}

          <div className="flex items-center justify-between">
            <Button
              size="sm"
              data-testid="btn-add-column"
              onClick={() => setColumns([...columns, { name: "", type: "text" }])}
            >
              Add column
            </Button>
            <Button
              tone="primary"
              size="sm"
              data-testid="btn-table-save"
              disabled={!dirty || columnErrors.some(Boolean) || saving}
              onClick={save}
            >
              {saving ? "Saving…" : "Save columns"}
            </Button>
          </div>
          <p className="text-micro text-ink-faint">
            Every table also has a <span className="ident">key</span> column. Agents upsert by it.
          </p>
        </div>
      ) : tab === "rows" ? (
        <div data-testid="inspector-table-rows">
          {rows.isPending ? (
            <p className="text-micro text-ink-faint">Loading rows…</p>
          ) : rows.error ? (
            <p className="text-micro text-[var(--danger)]">{(rows.error as Error).message}</p>
          ) : !rows.data?.rows.length ? (
            <p className="text-micro text-ink-faint">
              No rows yet. Anything an agent or endpoint writes shows up here.
            </p>
          ) : (
            <>
              <div className="overflow-x-auto border border-rule">
                <table className="w-full border-collapse text-micro">
                  <thead>
                    <tr>
                      {Object.keys(rows.data.rows[0]!).map((k) => (
                        <th
                          key={k}
                          className="ident border-b border-rule px-2 py-1 text-left font-normal text-ink-dim"
                        >
                          {k}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {rows.data.rows.map((row, i) => (
                      <tr key={i} data-testid="table-row">
                        {Object.values(row).map((v, j) => (
                          <td key={j} className="ident border-b border-rule px-2 py-1 align-top">
                            {v === null ? "—" : typeof v === "object" ? JSON.stringify(v) : String(v)}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              <div className="mt-2 flex items-center justify-between text-micro text-ink-faint">
                <span>
                  {offset + 1}–{Math.min(offset + PAGE, rows.data.total)} of {rows.data.total}
                </span>
                <div className="flex gap-1.5">
                  <Button
                    size="sm"
                    data-testid="btn-rows-prev"
                    disabled={offset === 0}
                    onClick={() => setOffset(Math.max(0, offset - PAGE))}
                  >
                    Previous
                  </Button>
                  <Button
                    size="sm"
                    data-testid="btn-rows-next"
                    disabled={offset + PAGE >= rows.data.total}
                    onClick={() => setOffset(offset + PAGE)}
                  >
                    Next
                  </Button>
                </div>
              </div>
            </>
          )}
        </div>
      ) : (
        <div className="flex flex-col gap-2" data-testid="inspector-table-query">
          <Field label="Read-only SQL" hint="SELECT only, scoped to this table.">
            <Textarea mono rows={4} data-testid="table-sql" value={sql} onChange={(e) => setSql(e.target.value)} />
          </Field>
          <div className="flex justify-end">
            <Button
              size="sm"
              data-testid="btn-run-query"
              onClick={async () => {
                setQueryError(null);
                try {
                  setQueryResult(await api.table(node.id).query(sql));
                } catch (e) {
                  setQueryResult(null);
                  setQueryError((e as Error).message);
                }
              }}
            >
              Run query
            </Button>
          </div>
          {queryError ? (
            <p className="text-micro text-[var(--danger)]" data-testid="query-error">
              {queryError}
            </p>
          ) : queryResult ? (
            <div className="max-h-56 overflow-auto border border-rule" data-testid="query-result">
              <table className="w-full border-collapse text-micro">
                <thead>
                  <tr>
                    {queryResult.columns.map((c) => (
                      <th
                        key={c}
                        className="ident border-b border-rule px-2 py-1 text-left font-normal text-ink-dim"
                      >
                        {c}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {queryResult.rows.map((row, i) => (
                    <tr key={i} data-testid="query-row">
                      {row.map((v, j) => (
                        <td key={j} className="ident border-b border-rule px-2 py-1 align-top">
                          {v === null ? "—" : typeof v === "object" ? JSON.stringify(v) : String(v)}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
              {queryResult.rows.length === 0 ? (
                <p className="px-2 py-1 text-micro text-ink-faint">No rows matched.</p>
              ) : null}
            </div>
          ) : null}
        </div>
      )}
    </>
  );
}
