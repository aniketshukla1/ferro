// Measured row heights for VirtualList.setHeights (word wrap, the diff view): prefix sums of
// heightOf(i) plus a binary search. On-demand: not part of the boot graph.

export function heightModel(heightOf) {
  let off = new Float64Array(1);
  return {
    build(count) {
      off = new Float64Array(count + 1);
      for (let i = 0; i < count; i++) off[i + 1] = off[i] + heightOf(i);
    },
    top: (i) => off[i],
    h: (i) => off[i + 1] - off[i],
    at(y) {
      let lo = 0;
      let hi = off.length - 2;
      if (y <= 0 || hi < 0) return 0;
      while (lo < hi) {
        const mid = (lo + hi + 1) >> 1;
        if (off[mid] <= y) lo = mid;
        else hi = mid - 1;
      }
      return lo;
    },
    get total() { return off[off.length - 1]; },
  };
}
