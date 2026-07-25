import { useLayoutEffect, useRef } from "react";

import type { ApplicationClientAction } from "./types";
import { Button } from "./ui/primitives";

export interface ComposerSuggestion {
  id: string;
  token: string;
  label: string;
  description: string;
  kind: "command" | "skill" | "agent";
  available: boolean;
  unavailableReason?: string;
  completesWithSpace?: boolean;
  clientAction?: ApplicationClientAction;
}

export function ComposerSuggestions({
  id,
  label,
  unavailableLabel,
  suggestions,
  activeIndex,
  query,
  onSelect,
  onActiveIndexChange,
}: {
  id: string;
  label: string;
  unavailableLabel: string;
  suggestions: ComposerSuggestion[];
  activeIndex: number;
  query: string;
  onSelect: (suggestion: ComposerSuggestion) => void;
  onActiveIndexChange: (index: number) => void;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const list = listRef.current;
    if (list === null) return;
    const activeOption = list.querySelector<HTMLElement>('[aria-selected="true"]');
    if (activeOption === null) return;
    const optionTop = activeOption.offsetTop;
    const optionBottom = optionTop + activeOption.offsetHeight;
    const visibleTop = list.scrollTop;
    const visibleBottom = visibleTop + list.clientHeight;
    if (optionTop < visibleTop) {
      list.scrollTop = activeOption === list.firstElementChild ? 0 : optionTop;
    } else if (optionBottom > visibleBottom) {
      list.scrollTop = optionBottom - list.clientHeight;
    }
  }, [activeIndex, suggestions.length]);
  if (suggestions.length === 0) return null;
  return (
    <div ref={listRef} id={id} className="composer-suggestions" role="listbox" aria-label={label}>
      {suggestions.map((suggestion, index) => (
        <Button
          id={`${id}-option-${index}`}
          key={suggestion.id}
          className={`composer-suggestion${index === activeIndex ? " is-active" : ""}`}
          variant="quiet"
          type="button"
          role="option"
          aria-selected={index === activeIndex}
          aria-disabled={!suggestion.available}
          onMouseEnter={() => onActiveIndexChange(index)}
          onClick={() => onSelect(suggestion)}
        >
          <span className={`composer-suggestion-token token-${suggestion.kind}`}>
            {highlightMatch(suggestion.token, query)}
          </span>
          <span className="composer-suggestion-copy">
            <strong>{suggestion.label}</strong>
            <small>{suggestion.available ? suggestion.description : suggestion.unavailableReason}</small>
          </span>
          {!suggestion.available ? <span className="composer-suggestion-state">{unavailableLabel}</span> : null}
        </Button>
      ))}
    </div>
  );
}

function highlightMatch(value: string, query: string) {
  const normalized = query.toLocaleLowerCase();
  const index = value.toLocaleLowerCase().indexOf(normalized);
  if (normalized === "" || index < 0) return value;
  return (
    <>
      {value.slice(0, index)}
      <mark>{value.slice(index, index + query.length)}</mark>
      {value.slice(index + query.length)}
    </>
  );
}
