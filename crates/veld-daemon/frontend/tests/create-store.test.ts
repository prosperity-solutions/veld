import { describe, it, expect } from "vitest";
import { createStore } from "../src/shared/create-store";

interface TestState {
  count: number;
  items: string[];
}

type TestAction =
  | { type: "INCREMENT" }
  | { type: "ADD_ITEM"; item: string }
  | { type: "SET_ITEMS"; items: string[] };

function testReducer(s: Readonly<TestState>, a: TestAction): TestState {
  switch (a.type) {
    case "INCREMENT":
      return { ...s, count: s.count + 1 };
    case "ADD_ITEM":
      return { ...s, items: [...s.items, a.item] };
    case "SET_ITEMS":
      return { ...s, items: a.items };
    default:
      return { ...s };
  }
}

describe("createStore", () => {
  it("initializes with frozen state", () => {
    const { getState } = createStore(testReducer, { count: 0, items: [] });
    expect(getState().count).toBe(0);
    expect(Object.isFrozen(getState())).toBe(true);
  });

  it("dispatch updates state immutably", () => {
    const { getState, dispatch } = createStore(testReducer, { count: 0, items: [] });
    const before = getState();
    dispatch({ type: "INCREMENT" });
    expect(getState().count).toBe(1);
    expect(getState()).not.toBe(before); // new object
  });

  it("state is frozen after dispatch", () => {
    const { getState, dispatch } = createStore(testReducer, { count: 0, items: [] });
    dispatch({ type: "INCREMENT" });
    expect(Object.isFrozen(getState())).toBe(true);
  });

  it("direct mutation throws on frozen state", () => {
    const { getState } = createStore(testReducer, { count: 0, items: [] });
    expect(() => {
      (getState() as any).count = 99;
    }).toThrow();
  });

  it("top-level array property cannot be reassigned", () => {
    const { getState } = createStore(testReducer, { count: 0, items: ["a"] });
    expect(() => {
      (getState() as any).items = ["replaced"];
    }).toThrow();
    // Note: shallow freeze — getState().items.push() won't throw at runtime
    // but TypeScript's Readonly<S> prevents it at compile time
  });

  it("dispatch is the only way to update", () => {
    const { getState, dispatch } = createStore(testReducer, { count: 0, items: [] });
    dispatch({ type: "ADD_ITEM", item: "hello" });
    expect(getState().items).toEqual(["hello"]);
    dispatch({ type: "ADD_ITEM", item: "world" });
    expect(getState().items).toEqual(["hello", "world"]);
  });

  it("multiple dispatches accumulate correctly", () => {
    const { getState, dispatch } = createStore(testReducer, { count: 0, items: [] });
    dispatch({ type: "INCREMENT" });
    dispatch({ type: "INCREMENT" });
    dispatch({ type: "INCREMENT" });
    expect(getState().count).toBe(3);
  });
});
