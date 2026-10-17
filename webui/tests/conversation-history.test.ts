import { describe, expect, test } from "bun:test";
import { historyStops, historyViewport } from "../src/lib/conversation-history";

describe("work history navigation", () => {
  test("dense indices retain contiguous ranges and both ends of history", () => {
    expect(historyStops(8, 3)).toEqual([
      {target: 0, first: 0, last: 2},
      {target: 4, first: 3, last: 5},
      {target: 7, first: 6, last: 7},
    ]);
    expect(historyStops(2, 20)).toEqual([{target: 0, first: 0, last: 0}, {target: 1, first: 1, last: 1}]);
    expect(historyStops(8, 0)).toEqual([]);
  });
  test("visibility covers every intersecting message, excluding header and composer", () => {
    const rects = [{top: -100, bottom: 52}, {top: 52, bottom: 180}, {top: 200, bottom: 440}, {top: 480, bottom: 700}];
    expect(historyViewport(rects, 52, 480)).toEqual({current: 1, visible: [1, 2]});
    expect(historyViewport(rects, 52, 200)).toEqual({current: 1, visible: [1]});
    expect(historyViewport(rects, 300, 480)).toEqual({current: 2, visible: [2]});
  });
  test("a long message remains current until it leaves the readable viewport", () => {
    expect(historyViewport([{top: -800, bottom: 200}, {top: 220, bottom: 280}], 100, 300)).toEqual({current: 0, visible: [0, 1]});
    expect(historyViewport([{top: 0, bottom: 0}], 100, 300)).toEqual({current: -1, visible: []});
    expect(historyViewport([{top: 0, bottom: 600}], 300, 200)).toEqual({current: -1, visible: []});
  });
});
