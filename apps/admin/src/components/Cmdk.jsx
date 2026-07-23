import React from "react";

/**
 * Command palette (⌘K) dialog. Presentational; parent owns open/query/index.
 */
export function Cmdk({
  open,
  query,
  setQuery,
  index,
  setIndex,
  filteredSections,
  onClose,
  onJump,
  onKeyDown,
  inputRef,
}) {
  if (!open) return null;

  return (
    <div
      className="cmdk-backdrop"
      role="presentation"
      onClick={onClose}
    >
      <div
        className="cmdk"
        role="dialog"
        aria-label="Jump to section"
        onClick={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          className="cmdk__input"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Jump to section…"
          aria-autocomplete="list"
        />
        <ul className="cmdk__list" role="listbox">
          {filteredSections.length === 0 ? (
            <li className="cmdk__empty">No matches</li>
          ) : (
            filteredSections.map((s, i) => (
              <li key={s.id}>
                <button
                  type="button"
                  className={
                    i === index ? "cmdk__item is-active" : "cmdk__item"
                  }
                  aria-selected={i === index}
                  onMouseEnter={() => setIndex(i)}
                  onClick={() => onJump(s.id)}
                >
                  {s.label}
                  <span className="cmdk__hint">#{s.id}</span>
                </button>
              </li>
            ))
          )}
        </ul>
      </div>
    </div>
  );
}
