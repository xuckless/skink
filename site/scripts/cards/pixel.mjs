// Skink's pixel avatar: a top-down skink, 16x16, in the logo's gradient.
const MAP = [
  '......aaaa......',
  '.....aaaaaa.....',
  '.....eaaaae.....',
  '......aaaa......',
  '..f....aa....f..',
  '...ff.aaaa.ff...',
  '.....aaaaaa.....',
  '......aaaa......',
  '......aaaa......',
  '..f...aaaa...f..',
  '...ff.aaaa.ff...',
  '.....aaaaaa.....',
  '.......aaa......',
  '........aa......',
  '.........aa.....',
  '........aa......',
];
const N = MAP.length;
const stops = ['#73DACA', '#7AA2F7', '#BB9AF7'];
const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
const mix = (t) => {
  const [a, b, f] = t < 0.5 ? [stops[0], stops[1], t / 0.5] : [stops[1], stops[2], (t - 0.5) / 0.5];
  const [A, B] = [hex(a), hex(b)];
  return '#' + A.map((v, i) => Math.round(v + (B[i] - v) * f).toString(16).padStart(2, '0')).join('');
};
export function pixelSvg(size, { bg = '#1A1B26', border = true } = {}) {
  const cell = size / (N + 4);
  const off = cell * 2;
  let r = '';
  MAP.forEach((row, y) =>
    [...row].forEach((ch, x) => {
      if (ch === '.') return;
      const fill = ch === 'e' ? '#C0CAF5' : mix((y + x * 0.25) / (N + 4));
      r += `<rect x="${(off + x * cell).toFixed(2)}" y="${(off + y * cell).toFixed(2)}" width="${(cell + 0.5).toFixed(2)}" height="${(cell + 0.5).toFixed(2)}" fill="${fill}"/>`;
    }),
  );
  const stroke = border
    ? `<rect x="${cell / 2}" y="${cell / 2}" width="${size - cell}" height="${size - cell}" rx="${cell * 3}" fill="none" stroke="url(#pg)" stroke-width="${cell / 1.6}"/>`
    : '';
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" width="${size}" height="${size}" shape-rendering="crispEdges"><defs><linearGradient id="pg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#73DACA"/><stop offset=".55" stop-color="#7AA2F7"/><stop offset="1" stop-color="#BB9AF7"/></linearGradient></defs><rect width="${size}" height="${size}" rx="${cell * 3.4}" fill="${bg}"/>${stroke}${r}</svg>`;
}
