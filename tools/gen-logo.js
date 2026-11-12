// Generates the Avery artwork (SVG) into assets/:
//   avery-icon.svg      classic icon: squircle tile with margin and shadow (README, DMG volume, macOS before 26)
//   avery-app-icon.svg  full-bleed square for the .app bundle: macOS 26 applies its own mask and edge to this
//   avery-mark.svg      the "A" and the flap on a transparent background (for light backgrounds)
// Usage: node tools/gen-logo.js
const fs = require('fs');
const path = require('path');

const out = path.join(__dirname, '..', 'assets');

// Squircle (superellipse n=5) as smooth cubic Beziers through sampled points (Catmull-Rom).
function squirclePath(cx, cy, half, n = 5, count = 96) {
  const P = [];
  for (let i = 0; i < count; i++) {
    const t = (i / count) * 2 * Math.PI, c = Math.cos(t), s = Math.sin(t);
    P.push([cx + half * Math.sign(c) * Math.pow(Math.abs(c), 2 / n), cy + half * Math.sign(s) * Math.pow(Math.abs(s), 2 / n)]);
  }
  const f = (v) => (Math.round(v * 10) / 10).toString();
  let d = `M${f(P[0][0])} ${f(P[0][1])}`;
  for (let i = 0; i < count; i++) {
    const p0 = P[(i - 1 + count) % count], p1 = P[i], p2 = P[(i + 1) % count], p3 = P[(i + 2) % count];
    d += `C${f(p1[0] + (p2[0] - p0[0]) / 6)} ${f(p1[1] + (p2[1] - p0[1]) / 6)} ${f(p2[0] - (p3[0] - p1[0]) / 6)} ${f(p2[1] - (p3[1] - p1[1]) / 6)} ${f(p2[0])} ${f(p2[1])}`;
  }
  return d + 'Z';
}
const SQ = squirclePath(512, 512, 412);

const A = { top: 270, bottom: 754, half: 180, stroke: 96 };
const C = { w: 72, y: 572, h: 66, stroke: 58, gap: 15 };
const legsD = `M${512 - A.half} ${A.bottom}L512 ${A.top}L${512 + A.half} ${A.bottom}`;
const chevD = `M${512 - C.w} ${C.y}L512 ${C.y + C.h}L${512 + C.w} ${C.y}`;
const stroke = (d, w, paint) => `<path d="${d}" fill="none" stroke="${paint}" stroke-width="${w}" stroke-linecap="round" stroke-linejoin="round"/>`;

const warm = (id) => `<linearGradient id="${id}" gradientUnits="userSpaceOnUse" x1="${512 - C.w - 30}" y1="${C.y - 30}" x2="${512 + C.w + 30}" y2="${C.y + C.h + 40}">
      <stop offset="0" stop-color="#FFD66E"/><stop offset="1" stop-color="#FF7A5A"/>
    </linearGradient>`;
const gapMask = `<mask id="gap" maskUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024">
      <rect width="1024" height="1024" fill="#fff"/>
      ${stroke(chevD, C.stroke + 2 * C.gap, '#000')}
    </mask>`;

function icon({ fullBleed }) {
  const scale = fullBleed ? 1024 / 824 : 1;
  const tile = fullBleed ? 'M0 0H1024V1024H0Z' : SQ;
  const glyph = `<g mask="url(#gap)"><g filter="url(#blur15)" opacity="0.5" transform="translate(0 20)">${stroke(legsD, A.stroke, '#12093e')}</g></g>
    <circle cx="512" cy="${C.y + C.h / 2 + 4}" r="150" fill="url(#flap)"/>
    <g mask="url(#gap)">${stroke(legsD, A.stroke, 'url(#ink)')}</g>
    <g filter="url(#blur15)" opacity="0.35" transform="translate(0 10)">${stroke(chevD, C.stroke, '#7a1d00')}</g>
    ${stroke(chevD, C.stroke, 'url(#warm)')}`;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <title>Avery</title>
  <defs>
    <linearGradient id="ink" gradientUnits="userSpaceOnUse" x1="0" y1="${A.top - 40}" x2="0" y2="${A.bottom + 40}">
      <stop offset="0" stop-color="#FFFFFF"/><stop offset="1" stop-color="#DCDFFF"/>
    </linearGradient>
    ${warm('warm')}
    ${gapMask}
    <linearGradient id="bg" x1="0.1" y1="0" x2="0.9" y2="1">
      <stop offset="0" stop-color="#46A0FF"/><stop offset="0.5" stop-color="#5B54F5"/><stop offset="1" stop-color="#26106A"/>
    </linearGradient>
    <radialGradient id="glow" cx="0.4" cy="0.34" r="0.66"><stop offset="0" stop-color="#fff" stop-opacity="0.32"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></radialGradient>
    <radialGradient id="vig" cx="0.5" cy="0.5" r="0.75"><stop offset="0.6" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#0a0630" stop-opacity="0.35"/></radialGradient>
    <linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.28"/><stop offset="0.5" stop-color="#fff" stop-opacity="0"/></linearGradient>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.55"/><stop offset="0.4" stop-color="#fff" stop-opacity="0.04"/><stop offset="1" stop-color="#000" stop-opacity="0.28"/></linearGradient>
    <radialGradient id="flap" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#FFB45E" stop-opacity="0.55"/><stop offset="1" stop-color="#FFB45E" stop-opacity="0"/></radialGradient>
    <filter id="blur22" filterUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024"><feGaussianBlur stdDeviation="22"/></filter>
    <filter id="blur15" filterUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024"><feGaussianBlur stdDeviation="15"/></filter>
    <clipPath id="tile"><path d="${tile}"/></clipPath>
  </defs>
  ${fullBleed ? '' : `<path d="${tile}" fill="#000" opacity="0.38" filter="url(#blur22)" transform="translate(0 16)"/>`}
  <path d="${tile}" fill="url(#bg)"/>
  <g clip-path="url(#tile)">
    <rect width="1024" height="1024" fill="url(#glow)"/>
    <rect width="1024" height="1024" fill="url(#vig)"/>
    <rect width="1024" height="560" fill="url(#sheen)"/>
  </g>
  ${fullBleed ? '' : `<path d="${tile}" fill="none" stroke="url(#rim)" stroke-width="3"/>`}
  <g transform="translate(512 512) scale(${scale.toFixed(4)}) translate(-512 -512)">
    ${glyph}
  </g>
</svg>
`;
}

const mark = `<svg xmlns="http://www.w3.org/2000/svg" width="480" height="600" viewBox="272 210 480 600">
  <title>Avery</title>
  <defs>
    <linearGradient id="inkdark" gradientUnits="userSpaceOnUse" x1="260" y1="${A.top - 40}" x2="760" y2="${A.bottom + 40}">
      <stop offset="0" stop-color="#46A0FF"/><stop offset="0.5" stop-color="#5B54F5"/><stop offset="1" stop-color="#26106A"/>
    </linearGradient>
    ${warm('warm')}
    ${gapMask}
  </defs>
  <g mask="url(#gap)">${stroke(legsD, A.stroke, 'url(#inkdark)')}</g>
  ${stroke(chevD, C.stroke, 'url(#warm)')}
</svg>
`;

fs.writeFileSync(path.join(out, 'avery-icon.svg'), icon({ fullBleed: false }));
fs.writeFileSync(path.join(out, 'avery-app-icon.svg'), icon({ fullBleed: true }));
fs.writeFileSync(path.join(out, 'avery-mark.svg'), mark);
console.log('assets/avery-icon.svg, assets/avery-app-icon.svg, assets/avery-mark.svg geschrieben');
