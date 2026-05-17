/* Kashot landing page — interactive layer.
 *
 *  - boot screen fade
 *  - cursor-tracked spotlight
 *  - parallax grid-floor offset (scroll)
 *  - scroll-scrubbed scale on the hero screenshot frame
 *  - sticky scrollytelling step-tracker for #showcase
 *  - reveal-on-scroll (IntersectionObserver, fire-once)
 *  - counter tick-up on first reveal
 *  - smooth in-page anchors
 *  - GitHub release fetch → updates download URL + version label
 *
 * No frameworks. One closure. One requestAnimationFrame loop.
 */

(function () {
  'use strict';

  const root  = document.documentElement;
  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const coarsePtr    = window.matchMedia('(pointer: coarse)').matches;

  root.setAttribute('data-theme', 'tron');

  // ── Boot fade ─────────────────────────────────────────────────────────
  window.addEventListener('load', () => {
    setTimeout(() => document.body.classList.add('booted'), 550);
  });

  // ── Smooth scroll for in-page anchors ─────────────────────────────────
  document.querySelectorAll('a[href^="#"]').forEach(a => {
    a.addEventListener('click', (e) => {
      const id = a.getAttribute('href').slice(1);
      if (!id) return;
      const t = document.getElementById(id);
      if (!t) return;
      e.preventDefault();
      t.scrollIntoView({ behavior: 'smooth', block: 'start' });
    });
  });

  // ── Cursor spotlight ──────────────────────────────────────────────────
  const glow = document.querySelector('.cursor-glow');
  if (glow && !coarsePtr && !reduceMotion) {
    let tx = -200, ty = -200, x = -200, y = -200, raf = 0;
    document.addEventListener('mousemove', (e) => {
      tx = e.clientX; ty = e.clientY;
      if (!raf) raf = requestAnimationFrame(tickGlow);
    }, { passive: true });
    function tickGlow() {
      // ease toward target so the spotlight has a little drag
      x += (tx - x) * 0.18;
      y += (ty - y) * 0.18;
      glow.style.setProperty('--x', x + 'px');
      glow.style.setProperty('--y', y + 'px');
      if (Math.abs(tx - x) > 0.5 || Math.abs(ty - y) > 0.5) {
        raf = requestAnimationFrame(tickGlow);
      } else {
        raf = 0;
      }
    }
  }

  // ── Scroll-driven layer (rAF-throttled) ───────────────────────────────
  const gridFloor    = document.querySelector('.grid-floor');
  const scrubScale   = document.querySelector('[data-scrub-scale]');
  const scrubFade    = document.querySelector('[data-scrub-fade]');
  // `.showcase-shell` is the grid container; data-step now lives there
  // (previously on `.showcase-stage` which was the all-sticky parent we
  // removed when splitting the layout so only the left frame stays pinned).
  const showcaseStg  = document.querySelector('.showcase-shell');
  const showcaseSteps= Array.from(document.querySelectorAll('.showcase-step'));

  let pending = false;

  function onScroll() {
    if (pending) return;
    pending = true;
    requestAnimationFrame(() => {
      const y = window.scrollY || window.pageYOffset || 0;

      // 1. parallax grid floor (slow drift independent of CSS animation)
      if (gridFloor && !reduceMotion) {
        gridFloor.style.setProperty('--gridY', (y * 0.15) + 'px');
      }

      // 2. hero subtitle / CTAs fade as the hero leaves the viewport
      if (scrubFade) {
        const h = scrubFade.offsetHeight || 1;
        const p = Math.min(1, Math.max(0, y / (h * 0.9)));
        scrubFade.style.setProperty('opacity', String(1 - p * 0.85));
      }

      // 3. screenshot scrubbed scale — grows as you scroll into it
      if (scrubScale) {
        const r = scrubScale.getBoundingClientRect();
        const vh = window.innerHeight;
        // progress: 0 when section just enters from below, 1 when it's centered
        let p = 1 - (r.top + r.height * 0.4) / vh;
        p = Math.min(1, Math.max(0, p));
        const s = 0.94 + p * 0.06;     // 0.94 → 1.00
        const t = (1 - p) * 12;         // 12px → 0
        scrubScale.style.transform = `translateY(${t}px) scale(${s})`;
      }

      pending = false;
    });
  }

  // ── Showcase step tracker ─────────────────────────────────────────────
  // Pick whichever step's bounding rect is closest to the viewport vertical
  // centre, and mirror that into both the step's `.active` class (drives the
  // green left rule + brightening) and into `data-step` on the shell (drives
  // the SVG selection/annotation reveals inside the sticky frame on the
  // left). IntersectionObserver tells us *when* to recompute; the actual
  // pick uses geometric distance so it works regardless of step heights.
  if (showcaseStg && showcaseSteps.length) {
    const pickActive = () => {
      const mid = window.innerHeight / 2;
      let bestIdx = 0, bestDist = Infinity;
      showcaseSteps.forEach((el, i) => {
        const r = el.getBoundingClientRect();
        const c = r.top + r.height / 2;
        const d = Math.abs(c - mid);
        if (d < bestDist) { bestDist = d; bestIdx = i; }
      });
      const step = bestIdx + 1;
      // No early-exit on data-step match — the HTML pre-sets `data-step="1"`
      // for the initial paint, so an early-exit would skip the very first
      // `.active` class application when the page lands on step 1.
      showcaseStg.dataset.step = String(step);
      showcaseSteps.forEach((el, i) => {
        el.classList.toggle('active', i === bestIdx);
      });
    };
    if ('IntersectionObserver' in window) {
      // We don't actually use the IO entries — we just use IO as a
      // cheap "viewport activity" signal so we recompute on scroll without
      // adding another scroll listener.
      const io = new IntersectionObserver(pickActive, {
        rootMargin: '-30% 0px -30% 0px',
        threshold: [0, 0.5, 1],
      });
      showcaseSteps.forEach(el => io.observe(el));
    }
    window.addEventListener('scroll', pickActive, { passive: true });
    window.addEventListener('resize', pickActive, { passive: true });
    pickActive();
  }

  window.addEventListener('scroll', onScroll, { passive: true });
  window.addEventListener('resize', onScroll, { passive: true });
  onScroll();

  // ── Reveal on scroll ──────────────────────────────────────────────────
  const revealEls = document.querySelectorAll('.reveal');
  if ('IntersectionObserver' in window && !reduceMotion) {
    const io = new IntersectionObserver((entries) => {
      entries.forEach(e => {
        if (e.isIntersecting) {
          e.target.classList.add('in');
          io.unobserve(e.target);
        }
      });
    }, { rootMargin: '-8% 0px -8% 0px', threshold: 0.05 });
    revealEls.forEach(el => io.observe(el));
  } else {
    revealEls.forEach(el => el.classList.add('in'));
  }

  // ── Counter tick-up ───────────────────────────────────────────────────
  const counters = document.querySelectorAll('.counter');
  if ('IntersectionObserver' in window && !reduceMotion) {
    const cio = new IntersectionObserver((entries) => {
      entries.forEach(e => {
        if (e.isIntersecting) {
          tickCounter(e.target);
          cio.unobserve(e.target);
        }
      });
    }, { threshold: 0.4 });
    counters.forEach(el => cio.observe(el));
  } else {
    counters.forEach(el => { el.textContent = el.dataset.target || el.textContent; });
  }

  function tickCounter(el) {
    const target = parseInt(el.dataset.target, 10) || 0;
    const dur = 900;
    const start = performance.now();
    function step(now) {
      const t = Math.min(1, (now - start) / dur);
      // easeOutCubic
      const e = 1 - Math.pow(1 - t, 3);
      el.textContent = String(Math.round(target * e));
      if (t < 1) requestAnimationFrame(step);
      else el.textContent = String(target);
    }
    requestAnimationFrame(step);
  }

  // ── OS detection ──────────────────────────────────────────────────────
  // Picks Windows / macOS / Linux for the hero download button + sub-label
  // and highlights the matching card in the acquire grid. `navigator.platform`
  // is deprecated but every browser still implements it and on macOS / Linux
  // it is far more reliable than parsing userAgent. We treat iOS / Android as
  // "other" — the desktop app isn't a meaningful download there, so we leave
  // the default (Windows MSI) which mirrors current behavior on mobile.
  function detectOS() {
    const ua  = (navigator.userAgent || '').toLowerCase();
    const plt = (navigator.platform  || '').toLowerCase();
    if (/iphone|ipad|ipod|android/.test(ua)) return 'other';
    if (/mac|darwin/.test(plt) || /mac os x|macintosh/.test(ua)) return 'macos';
    if (/win/.test(plt) || /windows/.test(ua))                    return 'windows';
    if (/linux|x11|bsd/.test(plt) || /linux|x11|cros|ubuntu|fedora|arch|debian/.test(ua)) return 'linux';
    return 'other';
  }

  const detectedOS = detectOS();

  // Per-OS asset selectors and label suffixes. Each picker walks the release
  // asset list in priority order; the first match wins. Fallback URLs match
  // the static `href` attribute already on the buttons in index.html so the
  // page degrades cleanly if the GitHub API call fails.
  //
  // Same Rust binary on all three platforms as of v0.2.0 — the picker order
  // prefers the new canonical artifacts, with the legacy C# MSI/EXE/ZIP kept
  // as fallback for the Windows acquire-card "INSTALLER" buttons.
  const OS_CONFIG = {
    windows: {
      label:    'WINDOWS · ZIP',
      pickers:  [/windows.*x86_64\.zip$/i, /\.msi$/i, /\.exe$/i, /-portable\.zip$/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest/download/kashot-windows-x86_64.zip',
      cardId:   'WINDOWS',
    },
    macos: {
      label:    'MACOS · APPLE SILICON',
      pickers:  [/macos.*arm64/i, /macos.*aarch64/i, /macos.*x64/i, /macos/i, /\.dmg$/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest/download/Kashot-macos-arm64',
      cardId:   'MACOS',
    },
    linux: {
      label:    'LINUX · TAR.GZ',
      pickers:  [/linux.*x86_64\.tar\.gz$/i, /linux.*\.tar\.gz$/i, /\.AppImage$/i, /linux/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest/download/kashot-linux-x86_64.tar.gz',
      cardId:   'LINUX',
    },
    other: {
      label:    'CHOOSE BUILD',
      pickers:  [/windows.*x86_64\.zip$/i, /\.msi$/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest',
      cardId:   null,
    },
  };

  const heroBtn = document.getElementById('download-btn');
  const sub     = document.getElementById('download-sub');

  // Latest GitHub Release payload cached here once the fetch returns, so a
  // user clicking another OS gets the real asset URL too (not just the
  // static fallback). Until the fetch completes we fall back to defaults.
  let latestRelease = null;
  let activeOS      = null;

  function applyOS(os) {
    const cfg = OS_CONFIG[os] || OS_CONFIG.other;
    activeOS  = os;

    const tag = (latestRelease && (latestRelease.tag_name || latestRelease.name)) || 'v0.2';
    let href  = cfg.fallback;

    if (latestRelease && latestRelease.assets) {
      for (const re of cfg.pickers) {
        const hit = latestRelease.assets.find(a => re.test(a.name));
        if (hit) { href = hit.browser_download_url; break; }
      }
    }

    if (heroBtn) heroBtn.href = href;
    if (sub)     sub.textContent = `${tag} · ${cfg.label}`;

    // Hero platforms strip — mark the active card.
    document.querySelectorAll('.platform.is-detected').forEach(p => p.classList.remove('is-detected'));
    if (['windows', 'linux', 'macos'].includes(os)) {
      const match = document.querySelector(`.platform[data-os="${os}"]`);
      if (match) match.classList.add('is-detected');
    }

    // Acquire grid — spotlight the matching dl-card.
    if (cfg.cardId) {
      document.querySelectorAll('.dl-card-active').forEach(c => c.classList.remove('dl-card-active'));
      document.querySelectorAll('.dl-card').forEach(card => {
        const h3 = card.querySelector('h3');
        if (h3 && h3.textContent.trim().toUpperCase() === cfg.cardId) {
          card.classList.add('dl-card-active');
        }
      });
    }
  }

  // First paint uses auto-detect; the user can override by clicking any
  // platform card in the hero strip or any dl-card in the acquire grid.
  applyOS(detectedOS);

  function wireClick(el, os) {
    if (!el) return;
    el.style.cursor = 'pointer';
    el.setAttribute('role', 'button');
    el.setAttribute('tabindex', '0');
    el.setAttribute('aria-label', `Show ${os} install instructions`);
    const handler = (ev) => {
      ev.preventDefault();
      applyOS(os);
      // Scroll the acquire grid into view so the user sees the swap.
      const acquire = document.getElementById('acquire');
      if (acquire) acquire.scrollIntoView({ behavior: 'smooth', block: 'start' });
    };
    el.addEventListener('click', handler);
    el.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter' || ev.key === ' ') handler(ev);
    });
  }

  // Hero strip — every platform card overrides the detected OS.
  document.querySelectorAll('.platform[data-os]').forEach(p => {
    const os = p.getAttribute('data-os');
    if (['windows', 'linux', 'macos'].includes(os)) wireClick(p, os);
  });

  // Acquire grid — clicking the dl-card header (but not the buttons inside)
  // also re-targets the active OS. We attach to the <header> so the actual
  // download buttons keep their native click behaviour.
  document.querySelectorAll('.dl-card').forEach(card => {
    const h3 = card.querySelector('h3');
    if (!h3) return;
    const label = h3.textContent.trim().toLowerCase();
    if (['windows', 'linux', 'macos'].includes(label)) {
      wireClick(card.querySelector('header') || card, label);
    }
  });

  // ── GitHub release fetch ──────────────────────────────────────────────
  // Look up the real asset URL + version tag from the latest release. If
  // the API call fails (rate limit / offline), the static fallbacks above
  // keep working as long as a release exists.
  const REPO = 'singhpratech/kashot';

  fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
    headers: { 'Accept': 'application/vnd.github+json' }
  })
    .then(r => r.ok ? r.json() : Promise.reject(r.status))
    .then(release => {
      latestRelease = release;
      const tag = release.tag_name || release.name || 'v0.2';

      // Re-apply the active OS so the hero button + label update with the
      // real release URL + tag, not just the static fallback.
      applyOS(activeOS || detectedOS);

      // Bump every static "v0.1 / v0.2" version label on the page to the
      // real tag so the topbar and brand tag don't lie.
      document.querySelectorAll('[data-version]').forEach(el => {
        el.textContent = tag;
      });

      // Rewrite all `releases/latest/download/*` asset links to the actual
      // tagged URLs, so download links keep working even after future
      // releases shift "latest" to a tag that doesn't include this asset.
      const assets = release.assets || [];
      const byName = new Map(assets.map(a => [a.name, a.browser_download_url]));
      document.querySelectorAll('a[href*="releases/latest/download/"]').forEach(a => {
        const m = a.href.match(/releases\/latest\/download\/([^/?#]+)/);
        if (m && byName.has(m[1])) a.href = byName.get(m[1]);
      });
    })
    .catch(() => { /* keep static fallbacks */ });

})();
