/**
 * Popover and Menu — the only two surfaces in the product allowed elevation
 * (06 §14.4: "exactly two levels — flat, and overlay").
 *
 * Both are rendered into a portal at the document root rather than inside the
 * pane that opened them, because a dockview group clips its children and a menu
 * that gets clipped by its own pane is the classic docked-UI defect.
 */

import { For, type JSX, createEffect, createSignal, onCleanup } from "solid-js";
import { Portal } from "solid-js/web";
import { Icon, type IconName } from "./icons";

export interface PopoverProps {
  open: boolean;
  onClose: () => void;
  /** Screen coordinates of the anchor's bottom-left corner. */
  anchor: { x: number; y: number };
  children: JSX.Element;
  label: string;
}

export function Popover(props: PopoverProps): JSX.Element {
  let element: HTMLDivElement | undefined;

  createEffect(() => {
    if (!props.open) return;
    const onPointerDown = (event: PointerEvent): void => {
      if (element !== undefined && !element.contains(event.target as Node)) props.onClose();
    };
    // Escape is handled here and not through the keymap: a dismissal is not a
    // command, it has no id, and binding it would put a global entry in the trie
    // that shadows Escape for every pane underneath.
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        event.stopPropagation();
        props.onClose();
      }
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    onCleanup(() => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    });
  });

  return (
    <>
      {props.open ? (
        <Portal>
          <div
            ref={element}
            class="overlay popover"
            // biome-ignore lint/a11y/useSemanticElements: `<dialog>` renders in the top layer with its own backdrop and focus trapping, none of which a popover anchored to a pane control wants.
            role="dialog"
            aria-label={props.label}
            style={{ left: `${props.anchor.x}px`, top: `${props.anchor.y}px` }}
          >
            {props.children}
          </div>
        </Portal>
      ) : null}
    </>
  );
}

export interface MenuItem {
  id: string;
  label: string;
  icon?: IconName;
  accelerator?: string;
  disabled?: boolean;
  /** A separator is an item with no label; keeping it in the list keeps indices honest. */
  separator?: boolean;
}

export interface MenuProps {
  items: readonly MenuItem[];
  onSelect: (id: string) => void;
  label: string;
}

/**
 * Roving focus rather than `tabindex` on every row: a menu is one tab stop, and
 * Up/Down move within it. That is the platform convention on all three targets
 * and it is what a screen reader expects from `role="menu"`.
 */
export function Menu(props: MenuProps): JSX.Element {
  const selectable = (): MenuItem[] => props.items.filter((i) => i.separator !== true);
  const [active, setActive] = createSignal(0);

  const move = (delta: number): void => {
    const items = selectable();
    if (items.length === 0) return;
    let next = active();
    for (let step = 0; step < items.length; step++) {
      next = (next + delta + items.length) % items.length;
      if (items[next]?.disabled !== true) break;
    }
    setActive(next);
  };

  const onKeyDown = (event: KeyboardEvent): void => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      move(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      move(-1);
    } else if (event.key === "Enter") {
      const item = selectable()[active()];
      if (item !== undefined && item.disabled !== true) props.onSelect(item.id);
    }
  };

  return (
    <div class="menu" role="menu" aria-label={props.label} tabindex={0} onKeyDown={onKeyDown}>
      <For each={props.items}>
        {(item) =>
          item.separator === true ? (
            <hr class="menu__separator" />
          ) : (
            <button
              type="button"
              role="menuitem"
              class="menu__item"
              disabled={item.disabled}
              data-active={selectable()[active()]?.id === item.id ? "" : undefined}
              onClick={() => props.onSelect(item.id)}
              onPointerEnter={() => {
                const at = selectable().indexOf(item);
                if (at >= 0) setActive(at);
              }}
            >
              {item.icon === undefined ? null : <Icon name={item.icon} />}
              <span class="menu__label">{item.label}</span>
              {item.accelerator === undefined ? null : (
                <span class="menu__accel t-micro">{item.accelerator}</span>
              )}
            </button>
          )
        }
      </For>
    </div>
  );
}
