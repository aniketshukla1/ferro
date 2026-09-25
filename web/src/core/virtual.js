// VirtualList: fixed-height rows with keyed recycling and scaled scrolling.
// Only visible rows (+ overscan) exist in the DOM; rows that stay visible are never
// re-rendered, so native text selection and hover state survive scrolling.
// Content taller than MAX_PX is scaled so 5M-line files still scroll correctly
// (browsers cap element heights around 17–33M px).

const MAX_PX = 15_000_000;

export class VirtualList {
  /**
   * @param {{
   *   scroller: HTMLElement, rowHeight: number, count?: number, overscan?: number,
   *   create: () => HTMLElement, update: (el: HTMLElement, index: number) => void,
   *   sizer?: HTMLElement, onRange?: (first: number, last: number) => void,
   *   topPad?: number,
   * }} o
   */
  constructor(o) {
    this.scroller = o.scroller;
    this.rowHeight = o.rowHeight;
    this.overscan = o.overscan ?? 10;
    this.create = o.create;
    this.update = o.update;
    this.onRange = o.onRange;
    this.onPaint = o.onPaint; // after every paint/refresh (decorations that depend on mounted rows)
    this.topPad = o.topPad ?? 0;
    this.sizer = o.sizer || document.createElement('div');
    if (!o.sizer) {
      this.sizer.className = 'vl-sizer';
      this.scroller.appendChild(this.sizer);
    }
    this.count = 0;
    this.mounted = new Map(); // index -> element
    this.pool = [];
    this.first = 0;
    this.last = -1;
    this.raf = 0;
    this.onScroll = () => this.schedule();
    this.scroller.addEventListener('scroll', this.onScroll, { passive: true });
    this.ro = new ResizeObserver(() => this.schedule());
    this.ro.observe(this.scroller);
    this.setCount(o.count ?? 0);
  }

  get virtualHeight() { return this.count * this.rowHeight + this.topPad; }

  setCount(n) {
    this.count = Math.max(0, n | 0);
    const total = this.virtualHeight;
    this.scaled = total > MAX_PX;
    this.sizer.style.height = `${this.scaled ? MAX_PX : total}px`;
    // Drop mounted rows past the new end.
    for (const [i, el] of this.mounted) {
      if (i >= this.count) this.release(i, el);
    }
    this.schedule(true);
  }

  /** Map scrollTop to the virtual (unscaled) top. */
  virtualTop(scrollTop, viewH) {
    if (!this.scaled) return scrollTop;
    const range = MAX_PX - viewH;
    const vRange = this.virtualHeight - viewH;
    return range > 0 ? (scrollTop / range) * vRange : 0;
  }

  /** Map a virtual top to scrollTop. */
  scrollTopFor(vTop, viewH) {
    if (!this.scaled) return vTop;
    const range = MAX_PX - viewH;
    const vRange = this.virtualHeight - viewH;
    return vRange > 0 ? (vTop / vRange) * range : 0;
  }

  schedule(force = false) {
    if (force) this.dirty = true;
    if (this.raf) return;
    this.raf = requestAnimationFrame(() => {
      this.raf = 0;
      this.paint();
    });
  }

  release(i, el) {
    this.mounted.delete(i);
    el.hidden = true;
    el.__index = -1;
    this.pool.push(el);
  }

  paint() {
    const viewH = this.scroller.clientHeight;
    const st = this.scroller.scrollTop;
    const vTop = this.virtualTop(st, viewH) - this.topPad;
    const rh = this.rowHeight;
    const first = Math.max(0, Math.floor(vTop / rh) - this.overscan);
    const last = Math.min(this.count - 1, Math.ceil((vTop + viewH) / rh) + this.overscan);
    for (const [i, el] of this.mounted) {
      if (i < first || i > last) this.release(i, el);
    }
    const shift = this.scaled ? st - (vTop + this.topPad) : 0;
    const dirty = this.dirty;
    this.dirty = false;
    for (let i = first; i <= last; i++) {
      let el = this.mounted.get(i);
      if (!el) {
        el = this.pool.pop();
        if (!el) {
          el = this.create();
          this.sizer.appendChild(el);
        }
        el.hidden = false;
        el.__index = i;
        this.mounted.set(i, el);
        this.update(el, i);
      } else if (dirty) {
        this.update(el, i);
      }
      el.style.transform = `translateY(${i * rh + this.topPad + shift}px)`;
    }
    if (first !== this.first || last !== this.last) {
      this.first = first;
      this.last = last;
      this.onRange?.(first, last);
    }
    this.onPaint?.();
  }

  /** Re-render mounted rows (all, or those passing the filter). */
  refresh(filter) {
    for (const [i, el] of this.mounted) {
      if (!filter || filter(i)) this.update(el, i);
    }
    this.onPaint?.();
  }

  /** Visible range without overscan. */
  visibleRange() {
    const viewH = this.scroller.clientHeight;
    const vTop = this.virtualTop(this.scroller.scrollTop, viewH) - this.topPad;
    const first = Math.max(0, Math.floor(vTop / this.rowHeight));
    const last = Math.min(this.count - 1, Math.floor((vTop + viewH - 1) / this.rowHeight));
    return { first, last };
  }

  /**
   * Scroll so that index is visible.
   * @param {number} index
   * @param {'auto'|'center'|'start'} [align]
   */
  scrollToIndex(index, align = 'auto') {
    const viewH = this.scroller.clientHeight;
    const rh = this.rowHeight;
    const rowTop = index * rh + this.topPad;
    const curV = this.virtualTop(this.scroller.scrollTop, viewH);
    let vTop = curV;
    if (align === 'center') vTop = rowTop - viewH / 2 + rh / 2;
    else if (align === 'start') vTop = rowTop - rh;
    else if (rowTop < curV + rh) vTop = rowTop - rh;
    else if (rowTop + rh > curV + viewH - rh) vTop = rowTop + 2 * rh - viewH;
    vTop = Math.max(0, Math.min(vTop, this.virtualHeight - viewH));
    this.scroller.scrollTop = this.scrollTopFor(vTop, viewH);
    this.schedule();
  }

  elementFor(index) { return this.mounted.get(index) || null; }

  destroy() {
    cancelAnimationFrame(this.raf);
    this.scroller.removeEventListener('scroll', this.onScroll);
    this.ro.disconnect();
    this.sizer.remove();
    this.mounted.clear();
    this.pool = [];
  }
}
