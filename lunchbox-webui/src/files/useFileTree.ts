/**
 * What the person browsing has asked for (issue #195).
 *
 * The split this whole feature rests on: *intent* lives here, in a reducer
 * that no refetch ever touches — which folders are open, how the columns are
 * sorted, what is selected — while the *facts* live in the query cache, one
 * query per directory. That is why a folder refetched after a write
 * re-renders in place instead of collapsing, and why a stale tree is a cache
 * problem rather than a tree problem.
 */
import { useMemo, useReducer } from "react";
import { type NodeKey, type SortColumn, type SortSpec, isWithin } from "./tree";

export interface TreeState {
  expanded: ReadonlySet<NodeKey>;
  selected: NodeKey | null;
  sort: SortSpec;
  foldersFirst: boolean;
  showHidden: boolean;
  /**
   * Directories the person has asked to see more of, as a page count. Absent
   * means the default bound.
   */
  pageLimits: ReadonlyMap<NodeKey, number>;
}

type Action =
  | { type: "toggle"; key: NodeKey }
  | { type: "expand"; key: NodeKey }
  | { type: "collapse"; key: NodeKey }
  | { type: "select"; key: NodeKey | null }
  | { type: "sort"; column: SortColumn }
  | { type: "foldersFirst"; value: boolean }
  | { type: "showHidden"; value: boolean }
  | { type: "showMore"; key: NodeKey; pages: number }
  | { type: "forgetSubtree"; key: NodeKey };

const INITIAL: TreeState = {
  expanded: new Set(),
  selected: null,
  sort: { column: "name", direction: "asc" },
  foldersFirst: true,
  showHidden: false,
  pageLimits: new Map(),
};

function reducer(state: TreeState, action: Action): TreeState {
  switch (action.type) {
    case "toggle":
      return state.expanded.has(action.key)
        ? reducer(state, { type: "collapse", key: action.key })
        : reducer(state, { type: "expand", key: action.key });

    case "expand": {
      if (state.expanded.has(action.key)) return state;
      const expanded = new Set(state.expanded);
      expanded.add(action.key);
      return { ...state, expanded };
    }

    case "collapse": {
      if (!state.expanded.has(action.key)) return state;
      const expanded = new Set(state.expanded);
      // Only this node. Everything under it stays in the set so that
      // reopening the folder puts it back the way it was left — which is what
      // Finder does, and what `reachableExpanded` makes safe by refusing to
      // fetch anything whose parent is shut.
      expanded.delete(action.key);
      return { ...state, expanded };
    }

    case "select":
      return { ...state, selected: action.key };

    case "sort":
      return {
        ...state,
        sort:
          state.sort.column === action.column
            ? {
                column: action.column,
                direction: state.sort.direction === "asc" ? "desc" : "asc",
              }
            : { column: action.column, direction: "asc" },
      };

    case "foldersFirst":
      return { ...state, foldersFirst: action.value };

    case "showHidden":
      return { ...state, showHidden: action.value };

    case "showMore": {
      const pageLimits = new Map(state.pageLimits);
      pageLimits.set(action.key, action.pages);
      return { ...state, pageLimits };
    }

    case "forgetSubtree": {
      // Called after a delete or a move. Without it, a folder created later
      // with the same name arrives mysteriously pre-expanded, with a stale
      // subtree under it.
      const expanded = new Set(
        [...state.expanded].filter((key) => !isWithin(key, action.key)),
      );
      const pageLimits = new Map(
        [...state.pageLimits].filter(([key]) => !isWithin(key, action.key)),
      );
      const selected =
        state.selected && isWithin(state.selected, action.key)
          ? null
          : state.selected;
      return { ...state, expanded, pageLimits, selected };
    }

    default:
      return state;
  }
}

export interface FileTree {
  state: TreeState;
  toggle: (key: NodeKey) => void;
  expand: (key: NodeKey) => void;
  collapse: (key: NodeKey) => void;
  select: (key: NodeKey | null) => void;
  sortBy: (column: SortColumn) => void;
  setFoldersFirst: (value: boolean) => void;
  setShowHidden: (value: boolean) => void;
  showMore: (key: NodeKey, pages: number) => void;
  forgetSubtree: (key: NodeKey) => void;
}

export function useFileTree(initial: Partial<TreeState> = {}): FileTree {
  const [state, dispatch] = useReducer(reducer, { ...INITIAL, ...initial });

  return useMemo(
    () => ({
      state,
      toggle: (key) => dispatch({ type: "toggle", key }),
      expand: (key) => dispatch({ type: "expand", key }),
      collapse: (key) => dispatch({ type: "collapse", key }),
      select: (key) => dispatch({ type: "select", key }),
      sortBy: (column) => dispatch({ type: "sort", column }),
      setFoldersFirst: (value) => dispatch({ type: "foldersFirst", value }),
      setShowHidden: (value) => dispatch({ type: "showHidden", value }),
      showMore: (key, pages) => dispatch({ type: "showMore", key, pages }),
      forgetSubtree: (key) => dispatch({ type: "forgetSubtree", key }),
    }),
    [state],
  );
}

/** The reducer itself, for tests that would rather not mount a component. */
export { reducer as fileTreeReducer, INITIAL as initialTreeState };
