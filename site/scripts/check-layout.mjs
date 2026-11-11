// Screenshots pages at desktop and phone width and reports anything that
// sticks out past the viewport. Usage (with the built site served locally):
//   CHROMIUM=/usr/bin/chromium-browser node scripts/check-layout.mjs http://localhost:4400 out/ / /docs/
import { chromium } from 'playwright-core';
const b = await chromium.launch({ executablePath: process.env.CHROMIUM ?? '/usr/bin/chromium-browser' });
const [base, out, ...pages] = process.argv.slice(2);
for (const [w,h,tag] of [[1440,900,'d'],[390,844,'m']]) {
  const p = await b.newPage({ viewport: { width: w, height: h } });
  for (const path of pages) {
    await p.goto(base + path, { waitUntil: 'networkidle' });
    await p.waitForTimeout(800);
    const ov = await p.evaluate(() => {
      const bad = [];
      for (const el of document.querySelectorAll('body *')) {
        const r = el.getBoundingClientRect();
        if (r.width && r.right > innerWidth + 1 && getComputedStyle(el).position !== 'fixed') {
          let q = el.parentElement, clipped = false;
          while (q && q !== document.body) { const o = getComputedStyle(q).overflowX; if (o === 'auto' || o === 'hidden' || o === 'scroll' || o === 'clip') { clipped = true; break; } q = q.parentElement; }
          if (!clipped) bad.push(el.tagName + '.' + el.className.toString().slice(0, 30) + ' ' + Math.round(r.right));
        }
      }
      return [document.documentElement.scrollWidth - innerWidth, bad.slice(0, 5)];
    });
    const name = path.replace(/\//g,'_') || 'home';
    await p.screenshot({ path: `${out}/${name}-${tag}.png`, fullPage: true });
    console.log(name, tag, JSON.stringify(ov));
  }
}
await b.close();
