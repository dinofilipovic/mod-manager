import { useEffect, useMemo, useRef, useState } from "react";
import { api, errorMessage } from "../api";
import type { FomodRequest } from "../bindings/FomodRequest";
import type { FomodState } from "../bindings/FomodState";
import type { GroupKind } from "../bindings/GroupKind";
import type { InstallOutcome } from "../bindings/InstallOutcome";
import type { OptionKind } from "../bindings/OptionKind";
import type { Selections } from "../bindings/Selections";
import { Icon } from "./Icon";

const GROUP_HINT: Record<GroupKind, string> = {
  SelectExactlyOne: "Choose one",
  SelectAtMostOne: "Choose one or none",
  SelectAtLeastOne: "Choose at least one",
  SelectAll: "All of these are installed",
  SelectAny: "Choose any",
};

/**
 * Walks through a mod's FOMOD installer one step at a time. Steps can appear
 * or disappear depending on earlier choices, so the backend is asked which
 * steps are visible after every change.
 */
export function FomodWizard({
  request,
  onDone,
  onCancel,
}: {
  request: FomodRequest;
  onDone: (outcome: InstallOutcome) => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const installer = request.installer;
  const [selections, setSelections] = useState<Selections>(request.defaults);
  const [state, setState] = useState<FomodState | null>(null);
  const [position, setPosition] = useState(0);
  const [focused, setFocused] = useState<{ step: number; group: number; option: number } | null>(null);
  const [images, setImages] = useState<Record<string, string>>({});
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    ref.current?.showModal();
  }, []);

  useEffect(() => {
    let current = true;
    void api.fomodState(request.token, selections).then((s) => current && setState(s));
    return () => {
      current = false;
    };
  }, [request.token, selections]);

  const visible = state?.visible_steps ?? installer.steps.map((_, i) => i);
  const stepIndex = visible[Math.min(position, visible.length - 1)] ?? 0;
  const step = installer.steps[stepIndex];
  const isLast = position >= visible.length - 1;

  const kindOf = (s: number, g: number, o: number): OptionKind =>
    state?.option_kinds[s]?.[g]?.[o] ?? installer.steps[s].groups[g].options[o].kind;

  const focusedOption = focused
    ? installer.steps[focused.step]?.groups[focused.group]?.options[focused.option]
    : null;
  const imagePath = focusedOption?.image ?? (position === 0 && !focused ? installer.image : null);

  const loadImage = useMemo(
    () => (path: string) => {
      if (images[path]) return;
      void api
        .fomodImage(request.token, path)
        .then((url) => setImages((m) => ({ ...m, [path]: url })))
        .catch(() => {});
    },
    [images, request.token],
  );
  useEffect(() => {
    if (imagePath) loadImage(imagePath);
  }, [imagePath, loadImage]);

  const chosen = (g: number) => selections[stepIndex]?.[g] ?? [];

  const toggle = (g: number, o: number, kind: GroupKind) => {
    setSelections((prev) => {
      const next = installer.steps.map((s, si) => s.groups.map((_, gi) => [...(prev[si]?.[gi] ?? [])]));
      const current = next[stepIndex][g];
      if (kind === "SelectExactlyOne") next[stepIndex][g] = [o];
      else if (kind === "SelectAtMostOne") next[stepIndex][g] = current.includes(o) ? [] : [o];
      else if (kind === "SelectAll") return prev;
      else next[stepIndex][g] = current.includes(o) ? current.filter((x) => x !== o) : [...current, o].sort((a, b) => a - b);
      return next;
    });
    setFocused({ step: stepIndex, group: g, option: o });
  };

  const install = async () => {
    setInstalling(true);
    setError(null);
    try {
      onDone(await api.finishFomod(request.token, selections));
    } catch (e) {
      setError(errorMessage(e));
      setInstalling(false);
    }
  };

  const cancel = () => {
    void api.cancelFomod(request.token);
    onCancel();
  };

  const info = installer.info;
  return (
    <dialog ref={ref} className="dialog fomod" onCancel={(e) => (e.preventDefault(), cancel())}>
      <header className="fomod-head">
        <div>
          <h2>{info?.name ?? request.mod_name}</h2>
          <p className="muted small">
            {[info?.version && `v${info.version}`, info?.author && `by ${info.author}`].filter(Boolean).join(" · ") ||
              "Installer options"}
          </p>
          {request.preselected && (
            <p className="small preselected-note">
              <Icon name="check" size={12} /> Your choices from last time are preselected where they still fit.
            </p>
          )}
        </div>
        {visible.length > 1 && (
          <ol className="fomod-steps">
            {visible.map((s, i) => (
              <li key={s} className={i === position ? "active" : i < position ? "done" : ""}>
                {installer.steps[s].name || `Step ${i + 1}`}
              </li>
            ))}
          </ol>
        )}
      </header>

      <div className="fomod-body">
        <div className="fomod-groups">
          {step ? (
            step.groups.map((group, g) => (
              <fieldset key={g} className="fomod-group">
                <legend>
                  {group.name} <span className="muted small">· {GROUP_HINT[group.kind]}</span>
                </legend>
                {group.options.map((option, o) => {
                  const kind = kindOf(stepIndex, g, o);
                  const single = group.kind === "SelectExactlyOne" || group.kind === "SelectAtMostOne";
                  const disabled = kind === "NotUsable" || kind === "Required" || group.kind === "SelectAll";
                  return (
                    <label
                      key={o}
                      className={`fomod-option${chosen(g).includes(o) ? " chosen" : ""}${kind === "NotUsable" ? " unusable" : ""}`}
                      onMouseEnter={() => setFocused({ step: stepIndex, group: g, option: o })}
                    >
                      <input
                        type={single ? "radio" : "checkbox"}
                        name={`g-${stepIndex}-${g}`}
                        checked={chosen(g).includes(o)}
                        disabled={disabled}
                        onChange={() => toggle(g, o, group.kind)}
                      />
                      <span>{option.name}</span>
                      {kind === "Recommended" && <span className="badge badge-win">Recommended</span>}
                      {kind === "Required" && <span className="badge badge-update">Required</span>}
                      {kind === "NotUsable" && <span className="badge badge-lose">Not usable</span>}
                    </label>
                  );
                })}
              </fieldset>
            ))
          ) : (
            <p className="muted">This installer has no options; press Install.</p>
          )}
        </div>
        <aside className="fomod-preview">
          {imagePath && images[imagePath] ? (
            <img src={images[imagePath]} alt="" />
          ) : (
            <div className="fomod-noimage">
              <Icon name="layers" size={28} />
            </div>
          )}
          <p className="fomod-description">
            {focusedOption?.description || (position === 0 && info?.description) || "Hover an option to see what it does."}
          </p>
        </aside>
      </div>

      {(error || (isLast && state?.error)) && <p className="error small">{error ?? state?.error}</p>}
      <div className="dialog-actions">
        <button onClick={cancel} disabled={installing}>
          Cancel
        </button>
        <span className="spacer" />
        {position > 0 && (
          <button onClick={() => (setPosition((p) => p - 1), setFocused(null))} disabled={installing}>
            Back
          </button>
        )}
        {isLast ? (
          <button className="primary" onClick={install} disabled={installing || !!state?.error}>
            {installing ? "Installing…" : "Install"}
          </button>
        ) : (
          <button className="primary" onClick={() => (setPosition((p) => p + 1), setFocused(null))}>
            Next
          </button>
        )}
      </div>
    </dialog>
  );
}
