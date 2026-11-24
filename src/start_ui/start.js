// The start page: the mark drawn in dots, above one field.
//
// This script only draws. The field is a plain form and works without it. The
// light rests mostly in front of the mark and turns slowly; it follows the
// pointer while the pointer is over the page. With reduced motion the mark is
// drawn once, lit from the front, and redrawn only when the size or the colors
// change.

(() => {
  'use strict';

  // ---- Copied verbatim from design/shell-prototype.html, lines 343, 353-357, 386-387, 389-425,
  // 427-428, 430-449, 451-474, 476-497 and 508-517. Do not edit it here: change the prototype,
  // then copy the same lines again.
  const TAU = Math.PI * 2;

  const hex = (v) => {
    v = v.trim();
    if (/^#[0-9a-f]{3}$/i.test(v)) v = '#' + [...v.slice(1)].map((c) => c + c).join('');
    return /^#[0-9a-f]{6}$/i.test(v) ? v.toLowerCase() : null;
  };

  const norm = (x, y, z) => { const l = Math.hypot(x, y, z) || 1; return [x / l, y / l, z / l]; };
  const KEY_LIGHT = norm(-0.24, -0.28, 0.93);

  // The form: a bowl on top of two lobes,
  // 1.18 times as wide as tall. Each part is a superellipse and all three meet at the widest point, so
  // the sides run straight from one into the next. depth is how deep the cleft is, as a share of the height.
  const CLEFT = 0.15;
  const forms = {};
  function form(depth) {
    if (forms[depth]) return forms[depth];
    const H = HEIGHT, h = 0.4 * H, b = H - h, pt = 2.4, pb = 2.5;
    const ratio = (1 - (1 - depth / 0.4) ** pt) ** (1 / pt), c = ratio / (1 + ratio), a = 1 - c;
    const y0 = h - H / 2;
    const inside = (x, y) => (y >= y0
      ? Math.abs(x) ** pb + ((y - y0) / b) ** pb <= 1
      : (Math.abs(Math.abs(x) - c) / a) ** pt + ((y0 - y) / h) ** pt <= 1);
    const pw = (v, p) => Math.sign(v) * Math.abs(v) ** (2 / p);
    const N = 160, lobe = [];
    let outline = [];
    for (let i = 0; i < N; i++) { const u = (i / N) * Math.PI; outline.push([pw(Math.cos(u), pb), y0 + b * pw(Math.sin(u), pb)]); }
    const end = Math.acos(-((c / a) ** (pt / 2)));
    for (let i = 0; i <= N; i++) { const u = (i / N) * end; lobe.push([c + a * pw(Math.cos(u), pt), y0 - h * pw(Math.sin(u), pt)]); }
    for (const [x, y] of lobe) outline.push([-x, y]);
    for (let i = N - 1; i > 0; i--) outline.push(lobe[i]);
    // Built as in the sketch, lobes up, then turned over: the cleft is at the bottom.
    outline = outline.map(([x, y]) => [x, -y]);
    const within = (x, y) => inside(x, -y);
    // The nearest point on the outline, and how far the innermost point is from it. Both are for shading.
    const near = (x, y) => {
      let best = 9, bx = 0, by = 0;
      for (const q of outline) { const d = (q[0] - x) ** 2 + (q[1] - y) ** 2; if (d < best) { best = d; bx = q[0]; by = q[1]; } }
      return [Math.sqrt(best), bx, by];
    };
    let deepest = 0;
    for (let x = -1; x <= 1; x += 0.05) for (let y = -1; y <= 1; y += 0.05) if (within(x, y)) deepest = Math.max(deepest, near(x, y)[0]);
    return (forms[depth] = { outline, inside: within, near, deepest, height: H });
  }
  const HEIGHT = 2 / 1.18;
  // How deep to cut the cleft so it still shows when the drawing is coarse: never less than the sketch's.
  const cleftFor = (least) => Math.min(0.3, Math.max(CLEFT, Math.round((least / HEIGHT) * 100) / 100));

  // Sample points spread evenly over a disc, used to measure how much of a dot's cell the form covers.
  const SAMPLES = Array.from({ length: 40 }, (_, i) => { const r = Math.sqrt((i + 0.5) / 40), a = i * 2.399963229728653; return [r * Math.cos(a), r * Math.sin(a)]; });

  // The three grids. Each calls add(x, y) for every point of a grid whose step is 2 / n.
  function grid(pattern, n, add) {
    const s = 2 / n;
    if (pattern === 'spiral') {
      const count = Math.round(n * n * 1.1);
      for (let i = 0; i < count; i++) {
        const r = Math.sqrt((i + 0.5) / count) * 1.19, a = i * 2.399963229728653;
        add(r * Math.cos(a), r * Math.sin(a));
      }
    } else if (pattern === 'matrix') {
      // A column gap runs down the middle, in line with the cleft.
      const m = n + 2;
      for (let i = -m; i <= m; i++) for (let j = -m; j <= m; j++) add((i + 0.5) * s, (j + 0.5) * s);
    } else {
      // Honeycomb: every row sits half a step across from the row above. Of the grids tried,
      // this is the one that keeps the round top round; a diagonal grid cuts it to a point.
      const m = n + 3, rise = s * Math.sqrt(3) / 2;
      for (let j = -m; j <= m; j++) for (let i = -m; i <= m; i++) add((i + (j & 1 ? 0.5 : 0)) * s, j * rise);
    }
  }

  const cache = {};
  // Dot centers on and around the form. Each dot carries the direction its patch of surface faces,
  // as if the outline were inflated like a cushion, and how much of its cell the form covers.
  // A dot on the edge is drawn smaller in that proportion, which is what keeps the outline true.
  function layout(pattern, n) {
    const key = pattern + n;
    if (cache[key]) return cache[key];
    const pts = [];
    const s = 2 / n, cell = s * (pattern === 'honeycomb' ? 0.525 : 0.5642);
    // The cleft is at least two rows of dots deep, or it would not show.
    const F = form(cleftFor(1.75 * s));
    const add = (x, y) => {
      if (Math.abs(x) > 1 + s || Math.abs(y) > HEIGHT / 2 + s) return;
      let cover = 0;
      for (const [dx, dy] of SAMPLES) if (F.inside(x + dx * cell, y + dy * cell)) cover++;
      cover /= SAMPLES.length;
      if (cover < 0.2) return;
      const [dist, bx, by] = F.near(x, y);
      const within = F.inside(x, y), q = within ? 1 - Math.min(1, dist / F.deepest) : 1, out = (within ? -q : q) / (dist || 1);
      pts.push([x, y, (x - bx) * out, (y - by) * out, Math.sqrt(Math.max(0, 1 - q * q)), cover]);
    };
    grid(pattern, n, add);
    return (cache[key] = { pts, rmax: s * 0.5 });
  }

  // The form in dots. With lights, each dot's area follows how much light reaches it.
  // Without (flat), every dot is full size and only the edge dots shrink.
  function dotted(ctx, cx, cy, R, o) {
    const { pts, rmax } = layout(o.pattern || theme.pattern, o.n);
    const square = (o.shape || theme.shape) === 'square';
    const lights = o.lights || [KEY_LIGHT], amb = o.amb ?? 0.3, fill = o.fill ?? 0.88;
    ctx.beginPath();
    for (const [x, y, nx, ny, nz, cover] of pts) {
      let level = cover;
      if (!o.flat) {
        let lit = 0;
        for (const l of lights) lit = Math.max(lit, nx * l[0] + ny * l[1] + nz * l[2]);
        level *= amb * (0.5 + 0.5 * nz) + (1 - amb) * lit;
      }
      const r = rmax * R * Math.sqrt(Math.min(1, level)) * fill;
      if (r < 0.3) continue;
      const px = cx + x * R, py = cy + y * R;
      if (square) ctx.rect(px - r * 0.88, py - r * 0.88, r * 1.76, r * 1.76);
      else { ctx.moveTo(px + r, py); ctx.arc(px, py, r, 0, TAU); }
    }
    ctx.fill();
  }

  function fit(canvas) {
    const dpr = Math.min(window.devicePixelRatio || 1, 3);
    const w = canvas.clientWidth, h = canvas.clientHeight;
    const W = Math.round(w * dpr), H = Math.round(h * dpr);
    if (canvas.width !== W || canvas.height !== H) { canvas.width = W; canvas.height = H; }
    const ctx = canvas.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    return [ctx, w, h];
  }
  // ---- End of the copy.

  // The prototype's default art (its DEFAULTS, line 347): honeycomb, round dots, 9 across.
  const theme = { pattern: 'honeycomb', shape: 'round', density: 9 };

  const canvas = document.getElementById('hero');
  const root = document.documentElement;
  const motion = matchMedia('(prefers-reduced-motion: reduce)');
  const dark = matchMedia('(prefers-color-scheme: dark)');
  let ink = '#000000';
  const readInk = () => { ink = hex(getComputedStyle(root).getPropertyValue('--ink')) || '#000000'; };

  // From the prototype's drawLive, its start-page part (lines 643-664), and its light (616-618).
  let pointer = null;
  const light = [...KEY_LIGHT];
  function draw(t) {
    const still = motion.matches;
    const [ctx, w] = fit(canvas);
    if (!w) return;
    let target;
    if (pointer && !still) {
      const box = canvas.getBoundingClientRect();
      let x = (pointer[0] - (box.left + box.width / 2)) / (box.width * 0.75);
      let y = (pointer[1] - (box.top + box.height / 2)) / (box.width * 0.75);
      const l = Math.hypot(x, y);
      if (l > 0.92) { x *= 0.92 / l; y *= 0.92 / l; }
      target = norm(x, y, Math.sqrt(Math.max(0.06, 1 - x * x - y * y)));
    } else {
      const a = still ? 0 : t * 0.22;
      target = norm(-0.24 + Math.cos(a) * 0.2, -0.28 + Math.sin(a) * 0.14, 0.93);
    }
    const k = still ? 1 : 0.1;
    for (let i = 0; i < 3; i++) light[i] += (target[i] - light[i]) * k;
    ctx.fillStyle = ink;
    dotted(ctx, w / 2, w / 2, w * 0.48, { n: theme.density * 3 - 2, lights: [norm(...light)], amb: 0.5 });
  }

  // One frame at a time while motion is allowed; a page that is not shown gets no frames.
  const t0 = performance.now();
  let running = false;
  function frame(now) {
    if (motion.matches) {
      running = false;
      draw(0);
      return;
    }
    draw((now - t0) / 1000);
    requestAnimationFrame(frame);
  }
  function start() {
    if (motion.matches) draw(0);
    else if (!running) {
      running = true;
      requestAnimationFrame(frame);
    }
  }

  // Without motion, nothing redraws by itself: these do.
  const redraw = () => { if (motion.matches) draw(0); };
  dark.addEventListener('change', () => { readInk(); redraw(); });
  motion.addEventListener('change', start);
  addEventListener('resize', redraw);
  root.addEventListener('pointermove', (e) => { pointer = [e.clientX, e.clientY]; });
  root.addEventListener('pointerleave', () => { pointer = null; });

  readInk();
  start();
})();
