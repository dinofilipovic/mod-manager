import { useCallback, useEffect, useState } from "react";
import { api, errorMessage, reveal } from "../api";
import type { BackupReason } from "../bindings/BackupReason";
import type { SaveBackup } from "../bindings/SaveBackup";
import type { SavesInfo } from "../bindings/SavesInfo";
import { ConfirmDialog } from "../components/Dialog";
import { Icon } from "../components/Icon";
import { useToast } from "../components/Toasts";
import { formatBytes, timeAgo } from "../format";

const REASON_LABEL: Record<BackupReason, string> = {
  BeforeLaunch: "Before playing",
  Manual: "Manual",
  BeforeRestore: "Before a restore",
};

/** Snapshots of the game's saves, taken by hand or before each launch. */
export function SavesView({ instanceId, gameName }: { instanceId: string; gameName: string }) {
  const toast = useToast();
  const [info, setInfo] = useState<SavesInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<{ kind: "restore" | "delete"; backup: SaveBackup } | null>(null);

  const load = useCallback(async () => {
    try {
      setInfo(await api.savesInfo(instanceId));
    } catch (e) {
      toast("error", "Couldn't read save backups", errorMessage(e));
    }
  }, [instanceId, toast]);

  useEffect(() => {
    void load();
  }, [load]);

  const backupNow = async () => {
    setBusy(true);
    try {
      const made = await api.backupSaves(instanceId);
      toast(made ? "success" : "info", made ? "Saves backed up" : "No saves to back up yet");
      await load();
    } catch (e) {
      toast("error", "Backup failed", errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  if (!info) {
    return (
      <p className="status-line">
        <Icon name="refresh" className="spin" /> Reading saves…
      </p>
    );
  }

  return (
    <div className="health">
      <section className="card">
        <div className="card-head-row">
          <h2>Save backups</h2>
          <div className="row">
            {info.save_dir && (
              <button className="link-button" onClick={() => reveal(info.save_dir!)}>
                Open saves folder
              </button>
            )}
            <button className="primary" onClick={backupNow} disabled={busy || !info.save_dir}>
              <Icon name="download" /> Back up now
            </button>
          </div>
        </div>
        {!info.save_dir ? (
          <p className="muted">
            No save folder found for {gameName} yet. It appears after you save in the game for the first time.
          </p>
        ) : (
          <label className="check">
            <input
              type="checkbox"
              checked={info.auto_backup}
              onChange={async (e) => {
                await api.setAutoSaveBackup(instanceId, e.target.checked);
                await load();
              }}
            />
            Back up saves automatically when I press Play (keeps the last 10, only when saves changed)
          </label>
        )}
      </section>

      <section className="card">
        <h2>Backups</h2>
        {info.backups.length === 0 ? (
          <p className="muted">No backups yet.</p>
        ) : (
          <ul className="backups">
            {info.backups.map((b) => (
              <li key={b.id}>
                <div>
                  <strong>{new Date(b.created_at * 1000).toLocaleString()}</strong>
                  <p className="muted small">
                    {REASON_LABEL[b.reason]} · {timeAgo(b.created_at)} · {formatBytes(b.size_bytes)}
                  </p>
                </div>
                <div className="row">
                  <button onClick={() => setConfirm({ kind: "restore", backup: b })}>Restore</button>
                  <button className="icon-button" aria-label="Delete backup" onClick={() => setConfirm({ kind: "delete", backup: b })}>
                    <Icon name="trash" />
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      {confirm?.kind === "restore" && (
        <ConfirmDialog
          title="Restore these saves?"
          message={`Your current saves are replaced with the backup from ${new Date(confirm.backup.created_at * 1000).toLocaleString()}. They're backed up first, so you can undo this. Close the game before restoring.`}
          confirmLabel="Restore"
          onConfirm={async () => {
            try {
              await api.restoreSaves(instanceId, confirm.backup.id);
              toast("success", "Saves restored");
              await load();
            } catch (e) {
              toast("error", "Restore failed", errorMessage(e));
            }
          }}
          onClose={() => setConfirm(null)}
        />
      )}
      {confirm?.kind === "delete" && (
        <ConfirmDialog
          title="Delete this backup?"
          message="The backup file is deleted. Your current saves aren't affected."
          confirmLabel="Delete"
          danger
          onConfirm={async () => {
            await api.deleteSaveBackup(instanceId, confirm.backup.id);
            await load();
          }}
          onClose={() => setConfirm(null)}
        />
      )}
    </div>
  );
}
