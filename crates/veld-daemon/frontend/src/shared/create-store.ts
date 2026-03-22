/**
 * Generic store factory with Object.freeze enforcement.
 *
 * Creates a store with a reducer pattern where:
 * - `getState()` returns `Readonly<S>` — TypeScript prevents mutations at compile time
 * - `Object.freeze()` catches any remaining mutations at runtime
 * - `dispatch(action)` is the ONLY way to update state
 *
 * Usage:
 * ```ts
 * const { getState, dispatch } = createStore(myReducer, initialState);
 * dispatch({ type: "SET_MODE", mode: "draw" });
 * console.log(getState().mode); // "draw"
 * getState().mode = "x"; // TypeScript error + runtime freeze error
 * ```
 */
export interface Store<S, A> {
  getState: () => Readonly<S>;
  dispatch: (action: A) => void;
}

export function createStore<S extends object, A>(
  reducer: (state: Readonly<S>, action: A) => S,
  initialState: S,
): Store<S, A> {
  let state: Readonly<S> = Object.freeze({ ...initialState } as S);

  return {
    getState(): Readonly<S> {
      return state;
    },
    dispatch(action: A): void {
      const next = reducer(state, action);
      state = Object.freeze(next as S);
    },
  };
}
