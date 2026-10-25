import { useEffect, useRef, useState, type ReactNode } from "react";

/** A modal built on <dialog>, so focus trapping and Escape come for free. */
export function Dialog({
  title,
  children,
  onClose,
  actions,
}: {
  title: string;
  children: ReactNode;
  onClose: () => void;
  actions: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    ref.current?.showModal();
  }, []);
  return (
    <dialog ref={ref} className="dialog" onClose={onClose} onCancel={onClose}>
      <h2>{title}</h2>
      <div className="dialog-body">{children}</div>
      <div className="dialog-actions">{actions}</div>
    </dialog>
  );
}

export function ConfirmDialog({
  title,
  message,
  confirmLabel,
  danger,
  onConfirm,
  onClose,
}: {
  title: string;
  message: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Dialog
      title={title}
      onClose={onClose}
      actions={
        <>
          <button onClick={onClose}>Cancel</button>
          <button
            className={danger ? "danger" : "primary"}
            autoFocus
            onClick={() => {
              onConfirm();
              onClose();
            }}
          >
            {confirmLabel}
          </button>
        </>
      }
    >
      {message}
    </Dialog>
  );
}

export function PromptDialog({
  title,
  label,
  initial = "",
  confirmLabel,
  extra,
  onSubmit,
  onClose,
}: {
  title: string;
  label: string;
  initial?: string;
  confirmLabel: string;
  extra?: ReactNode;
  onSubmit: (value: string) => void;
  onClose: () => void;
}) {
  const [value, setValue] = useState(initial);
  const submit = () => {
    if (!value.trim()) return;
    onSubmit(value.trim());
    onClose();
  };
  return (
    <Dialog
      title={title}
      onClose={onClose}
      actions={
        <>
          <button onClick={onClose}>Cancel</button>
          <button className="primary" disabled={!value.trim()} onClick={submit}>
            {confirmLabel}
          </button>
        </>
      }
    >
      <label className="field">
        <span>{label}</span>
        <input
          autoFocus
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && submit()}
        />
      </label>
      {extra}
    </Dialog>
  );
}
