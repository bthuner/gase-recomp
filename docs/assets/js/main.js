/* gase-recomp — making-of. Vanilla JS, no dependencies, no tracking. */
(function () {
  "use strict";

  window.__gase = true;
  var doc = document.documentElement;
  var LANG = (doc.getAttribute("lang") || "en").slice(0, 2) === "fr" ? "fr" : "en";
  var reduceMQ = window.matchMedia ? window.matchMedia("(prefers-reduced-motion: reduce)") : { matches: false };
  var REDUCED = reduceMQ.matches;

  var T = {
    en: { copy: "Copy", copied: "Copied", done: " of 8 done", copyLabel: "Copy code to clipboard" },
    fr: { copy: "Copier", copied: "Copié", done: " sur 8 faites", copyLabel: "Copier le code" }
  }[LANG];

  function $$(sel, root) { return Array.prototype.slice.call((root || document).querySelectorAll(sel)); }
  function esc(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;"); }

  /* ------------------------------------------------------------------
     1. Tiny syntax highlighter (C, Rust, TOML, 68000 asm, Python, shell)
     ------------------------------------------------------------------ */
  var RULES = {
    c: [
      ["c", /\/\*[\s\S]*?\*\/|\/\/[^\n]*/y],
      ["s", /"(?:\\.|[^"\\\n])*"/y],
      ["k", /\b(?:if|else|return|goto|switch|case|break|default|static|inline|typedef|struct|const|while|for|extern|#include|#define)\b/y],
      ["t", /\b(?:void|bool|int|uint8_t|uint16_t|uint32_t|uint64_t|int8_t|int16_t|int32_t|GaseCpu)\b/y],
      ["n", /\b0x[0-9A-Fa-f]+[uU]?\b|\b\d+\b/y],
      ["f", /\b[A-Za-z_]\w*(?=\s*\()/y],
      ["l", /\bL_[0-9A-Fa-f]+\b/y]
    ],
    rs: [
      ["c", /\/\/[^\n]*|\/\*[\s\S]*?\*\//y],
      ["s", /"(?:\\.|[^"\\\n])*"/y],
      ["k", /\b(?:fn|let|mut|loop|match|if|else|return|continue|break|pub|use|mod|impl|for|in|while|struct|enum|self|Self|as|const|true|false|where|move)\b/y],
      ["t", /\b(?:u8|u16|u32|u64|i8|i16|i32|i64|usize|bool|Cx|Option|Some|None|Box|M68k|Machine)\b/y],
      ["n", /\b0x[0-9A-Fa-f_]+\b|\b\d[\d_]*\b/y],
      ["f", /\b[A-Za-z_]\w*!?(?=\s*\()/y]
    ],
    toml: [
      ["c", /#[^\n]*/y],
      ["k", /^\[[^\]\n]+\]/my],
      ["s", /"(?:\\.|[^"\\\n])*"/y],
      ["t", /\b[A-Za-z_][\w.-]*(?=\s*=)/y],
      ["n", /\b\d+\b/y]
    ],
    asm: [
      ["c", /;[^\n]*/y],
      ["k", /\b(?:move|movea|moveq|lea|bsr|jsr|rts|rte|bra|beq|bne|bcc|bcs|jmp|add|addq|adda|sub|subq|cmp|cmpi|tst|dbra|dbf|clr|and|or|btst|swap|ext)(?:\.[bwls])?\b/y],
      ["t", /\b(?:d[0-7]|a[0-7]|sp|pc|sr|ccr)\b/y],
      ["n", /#?\$[0-9A-Fa-f]+|#\d+|\b[0-9A-Fa-f]{8}\b/y]
    ],
    py: [
      ["c", /#[^\n]*/y],
      ["s", /[bfr]?"(?:\\.|[^"\\\n])*"|[bfr]?'(?:\\.|[^'\\\n])*'/y],
      ["k", /\b(?:def|return|for|in|if|else|elif|import|from|while|and|or|not|with|as|class)\b/y],
      ["t", /\b(?:bytes|bytearray|int|range|len|str)\b/y],
      ["n", /\b0x[0-9A-Fa-f_]+\b|\b\d+\b/y],
      ["f", /\b[A-Za-z_]\w*(?=\()/y]
    ],
    sh: [
      ["c", /#[^\n]*/y],
      ["k", /--[A-Za-z][\w-]*|-[oSB]\b/y],
      ["n", /\b0x[0-9A-Fa-f]+\b|\$[0-9A-Fa-f]{6,8}\b/y],
      ["s", /"[^"\n]*"/y]
    ]
  };

  function highlightLine(src, rules) {
    var out = "", i = 0, plain = "";
    outer: while (i < src.length) {
      for (var r = 0; r < rules.length; r++) {
        var re = rules[r][1];
        re.lastIndex = i;
        var m = re.exec(src);
        if (m && m[0].length) {
          if (plain) { out += esc(plain); plain = ""; }
          var cls = rules[r][0] === "l" ? "n" : rules[r][0];
          out += '<span class="tok-' + cls + '">' + esc(m[0]) + "</span>";
          i += m[0].length;
          continue outer;
        }
      }
      // consume one identifier/char at a time so keywords never match mid-word
      var w = /[A-Za-z_0-9]+|[\s\S]/y;
      w.lastIndex = i;
      var mm = w.exec(src);
      plain += mm[0];
      i += mm[0].length;
    }
    return out + esc(plain);
  }

  function highlight(text, lang) {
    if (lang === "sh") {
      return text.split("\n").map(function (line) {
        if (/^\$ /.test(line)) return '<span class="tok-p">$</span>' + highlightLine(line.slice(1), RULES.sh);
        if (/^#/.test(line)) return '<span class="tok-c">' + esc(line) + "</span>";
        return '<span class="tok-o">' + esc(line) + "</span>";
      }).join("\n");
    }
    var rules = RULES[lang];
    if (!rules) return esc(text);
    return highlightLine(text, rules);
  }

  try {
    $$("pre > code[data-lang]").forEach(function (el) {
      el.innerHTML = highlight(el.textContent, el.getAttribute("data-lang"));
    });
  } catch (e) { /* sticky-regex unsupported: keep plain text */ }

  /* copy buttons */
  $$(".code").forEach(function (block) {
    var bar = block.querySelector(".code__bar");
    var code = block.querySelector("pre code");
    if (!bar || !code) return;
    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "copy";
    btn.textContent = T.copy;
    btn.setAttribute("aria-label", T.copyLabel);
    bar.appendChild(btn);
    btn.addEventListener("click", function () {
      var text = code.textContent.replace(/^\$ /gm, "");
      var ok = function () {
        btn.textContent = T.copied;
        btn.classList.add("is-copied");
        setTimeout(function () { btn.textContent = T.copy; btn.classList.remove("is-copied"); }, 1600);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(ok, function () {});
      } else {
        var ta = document.createElement("textarea");
        ta.value = text; ta.style.position = "fixed"; ta.style.opacity = "0";
        document.body.appendChild(ta); ta.select();
        try { document.execCommand("copy"); ok(); } catch (e) {}
        document.body.removeChild(ta);
      }
    });
  });

  /* ------------------------------------------------------------------
     2. Reveal on scroll + word splitting for chapter titles
     ------------------------------------------------------------------ */
  $$("[data-split]").forEach(function (el) {
    var words = el.textContent.trim().split(/[ \t\n\r]+/);
    el.setAttribute("aria-label", el.textContent.trim());
    el.innerHTML = words.map(function (w, i) {
      return '<span class="w" aria-hidden="true"><span style="--i:' + i + '">' + esc(w) + "</span></span>";
    }).join(" ");
  });

  $$(".chapter__body > *, .chapter__titles > .kicker").forEach(function (el) {
    if (!el.hasAttribute("data-reveal")) el.setAttribute("data-reveal", "");
  });
  $$(".steps > li, .platforms > *, .compare > *, .loop > li").forEach(function (el) {
    el.setAttribute("data-reveal", "");
    var idx = Array.prototype.indexOf.call(el.parentNode.children, el);
    el.style.setProperty("--d", (idx * 0.09) + "s");
  });

  var revealEls = $$("[data-reveal], [data-split]");
  if (REDUCED || !("IntersectionObserver" in window)) {
    revealEls.forEach(function (el) { el.classList.add("is-in"); });
  } else {
    var ro = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) {
        if (en.isIntersecting) { en.target.classList.add("is-in"); ro.unobserve(en.target); }
      });
    }, { rootMargin: "0px 0px -8% 0px", threshold: 0.08 });
    revealEls.forEach(function (el) { ro.observe(el); });
  }

  /* ------------------------------------------------------------------
     3. Chapter progress: rail, header label, reading bar
     ------------------------------------------------------------------ */
  var chapters = $$(".chapter[id]");
  var railLinks = $$(".rail a");
  var nowNum = document.querySelector("[data-now-num]");
  var nowTitle = document.querySelector("[data-now-title]");
  var bar = document.querySelector(".read-progress");
  var rail = document.querySelector(".rail");
  var hero = document.querySelector(".hero");
  if (rail && hero && "IntersectionObserver" in window) {
    new IntersectionObserver(function (en) {
      rail.classList.toggle("is-visible", !en[0].isIntersecting);
    }, { threshold: 0.35 }).observe(hero);
  } else if (rail) rail.classList.add("is-visible");

  function setActive(id) {
    var found = false;
    railLinks.forEach(function (a) {
      var on = a.getAttribute("href") === "#" + id;
      a.classList.toggle("is-active", on);
      if (on) { a.setAttribute("aria-current", "step"); found = true; } else a.removeAttribute("aria-current");
    });
    var passed = true;
    railLinks.forEach(function (a) {
      if (a.classList.contains("is-active")) passed = false;
      a.classList.toggle("is-done", passed && found);
    });
    chapters.forEach(function (c) { c.classList.toggle("is-current", c.id === id); });
    var ch = document.getElementById(id);
    if (ch && nowNum && nowTitle) {
      nowNum.textContent = ch.getAttribute("data-num") || "";
      nowTitle.textContent = ch.getAttribute("data-title") || "";
    }
  }

  if ("IntersectionObserver" in window && chapters.length) {
    var co = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) { if (en.isIntersecting) setActive(en.target.id); });
    }, { rootMargin: "-45% 0px -54% 0px", threshold: 0 });
    chapters.forEach(function (c) { co.observe(c); });
  }

  var maxScroll = 1, ticking = false;
  function measure() { maxScroll = Math.max(1, document.documentElement.scrollHeight - window.innerHeight); }
  function paintProgress() {
    ticking = false;
    if (bar) bar.style.transform = "scaleX(" + Math.min(1, Math.max(0, window.scrollY / maxScroll)).toFixed(4) + ")";
  }
  window.addEventListener("scroll", function () {
    if (!ticking) { ticking = true; requestAnimationFrame(paintProgress); }
  }, { passive: true });
  window.addEventListener("resize", function () { measure(); paintProgress(); });
  window.addEventListener("load", function () { measure(); paintProgress(); });
  measure(); paintProgress();

  /* ------------------------------------------------------------------
     4. Opcode decoder toy
     ------------------------------------------------------------------ */
  var FIELD_COLORS = ["#FF4900", "#2492FF", "#6DFF92", "#FFDB48", "#FFB6DB", "#B6B6FF"];
  // [highBit, width, en, fr]
  var OPS = [
    {
      id: "3080", addr: "00000204", words: ["3080"], asm: "move.w d0,(a0)",
      fields: [
        [15, 2, "Bits 15–14 = 00 and size ≠ 00: the MOVE family", "Bits 15–14 = 00 et taille ≠ 00 : la famille MOVE"],
        [13, 2, "Size 11 = word (01 = byte, 10 = long)", "Taille 11 = mot (01 = octet, 10 = long)"],
        [11, 3, "Destination register 0", "Registre destination 0"],
        [8, 3, "Destination mode 010 = (An) → (a0). Note: register before mode!", "Mode destination 010 = (An) → (a0). Le registre vient avant le mode !"],
        [5, 3, "Source mode 000 = Dn", "Mode source 000 = Dn"],
        [2, 3, "Source register 0 → d0", "Registre source 0 → d0"]
      ],
      ext: ["No extension word: 2 bytes, 8 cycles.", "Aucun mot d'extension : 2 octets, 8 cycles."]
    },
    {
      id: "D041", addr: "00000206", words: ["D041"], asm: "add.w d1,d0",
      fields: [
        [15, 4, "Line D = ADD / ADDA / ADDX", "Ligne D = ADD / ADDA / ADDX"],
        [11, 3, "Data register 0 → d0", "Registre de données 0 → d0"],
        [8, 3, "Opmode 001 = word, <ea> + Dn → Dn", "Opmode 001 = mot, <ea> + Dn → Dn"],
        [5, 3, "EA mode 000 = Dn", "Mode EA 000 = Dn"],
        [2, 3, "EA register 1 → d1", "Registre EA 1 → d1"]
      ],
      ext: ["Sets X N Z V C. 4 cycles.", "Met à jour X N Z V C. 4 cycles."]
    },
    {
      id: "41F9", addr: "00000208", words: ["41F9", "00FF", "0000"], asm: "lea ($00FF0000).l,a0",
      fields: [
        [15, 4, "Line 4 = miscellaneous", "Ligne 4 = divers"],
        [11, 3, "Address register 0 → a0", "Registre d'adresse 0 → a0"],
        [8, 3, "111 = LEA", "111 = LEA"],
        [5, 3, "EA mode 111 = 'special' (absolute, PC-relative, immediate)", "Mode EA 111 = « spécial » (absolu, relatif au PC, immédiat)"],
        [2, 3, "Register 001 in mode 7 = abs.L: a 32-bit address follows", "Registre 001 en mode 7 = abs.L : une adresse 32 bits suit"]
      ],
      ext: ["Two extension words 00FF 0000 → $00FF0000, the start of work RAM. 6 bytes total.", "Deux mots d'extension 00FF 0000 → $00FF0000, le début de la RAM. 6 octets au total."]
    },
    {
      id: "6100", addr: "00000300", words: ["6100", "0010"], asm: "bsr.w $00000312",
      fields: [
        [15, 4, "Line 6 = BRA / BSR / Bcc", "Ligne 6 = BRA / BSR / Bcc"],
        [11, 4, "Condition 0001 = BSR (0000 = BRA, others = Bcc)", "Condition 0001 = BSR (0000 = BRA, les autres = Bcc)"],
        [7, 8, "8-bit displacement 0 → a 16-bit displacement follows", "Déplacement 8 bits nul → un déplacement 16 bits suit"]
      ],
      ext: ["Extension word $0010: target = $00000302 + $10 = $00000312. A new function entry for the analyser.", "Mot d'extension $0010 : cible = $00000302 + $10 = $00000312. Une nouvelle fonction pour l'analyseur."]
    },
    {
      id: "4E75", addr: "00000318", words: ["4E75"], asm: "rts",
      fields: [
        [15, 4, "Line 4 = miscellaneous", "Ligne 4 = divers"],
        [11, 12, "Fixed pattern $E75 = RTS: pop the return address into PC", "Motif fixe $E75 = RTS : dépile l'adresse de retour dans PC"]
      ],
      ext: ["Ends the block: control flow leaves the function here.", "Termine le bloc : le flot de contrôle quitte la fonction ici."]
    }
  ];

  var dec = document.querySelector("[data-decoder]");
  if (dec) {
    var tabs = document.createElement("div");
    tabs.className = "decoder__tabs";
    tabs.setAttribute("role", "tablist");
    tabs.setAttribute("aria-label", dec.getAttribute("data-label") || "Opcodes");
    var panel = dec.querySelector(".decoder__body");
    panel.id = "decoder-panel";
    panel.setAttribute("role", "tabpanel");
    panel.setAttribute("tabindex", "0");
    panel.setAttribute("aria-live", "polite");
    var buttons = OPS.map(function (op, i) {
      var b = document.createElement("button");
      b.type = "button";
      b.id = "dec-tab-" + op.id;
      b.setAttribute("role", "tab");
      b.setAttribute("aria-controls", "decoder-panel");
      b.textContent = op.words.join(" ");
      b.addEventListener("click", function () { select(i, false); });
      b.addEventListener("keydown", function (e) {
        var k = e.key, n = null;
        if (k === "ArrowRight") n = (i + 1) % OPS.length;
        else if (k === "ArrowLeft") n = (i - 1 + OPS.length) % OPS.length;
        else if (k === "Home") n = 0;
        else if (k === "End") n = OPS.length - 1;
        if (n !== null) { e.preventDefault(); select(n, true); }
      });
      tabs.appendChild(b);
      return b;
    });
    dec.insertBefore(tabs, panel);

    function select(i, focus) {
      var op = OPS[i];
      buttons.forEach(function (b, j) {
        b.setAttribute("aria-selected", j === i ? "true" : "false");
        b.tabIndex = j === i ? 0 : -1;
      });
      if (focus) buttons[i].focus();
      panel.setAttribute("aria-labelledby", buttons[i].id);
      var word = parseInt(op.words[0], 16);
      var bitsHtml = "", numsHtml = "";
      var colorOf = [];
      op.fields.forEach(function (f, fi) {
        for (var b = f[0]; b > f[0] - f[1]; b--) colorOf[b] = fi;
      });
      for (var b = 15; b >= 0; b--) {
        var fi = colorOf[b];
        bitsHtml += '<span class="bit" data-f="' + fi + '" style="--fc:' + FIELD_COLORS[fi % FIELD_COLORS.length] + '">' + ((word >> b) & 1) + "</span>";
        numsHtml += "<span>" + b + "</span>";
      }
      var fieldsHtml = op.fields.map(function (f, fi) {
        var v = "";
        for (var b = f[0]; b > f[0] - f[1]; b--) v += (word >> b) & 1;
        return '<li data-f="' + fi + '" style="--fc:' + FIELD_COLORS[fi % FIELD_COLORS.length] + '"><span class="sw"></span><span class="val">' + v + "</span><span>" + esc(LANG === "fr" ? f[3] : f[2]) + "</span></li>";
      }).join("");
      var hex = '<span class="hex">' + op.addr + "</span>" + op.words[0] +
        (op.words.length > 1 ? ' <span class="ext">' + op.words.slice(1).join(" ") + "</span>" : "");
      panel.innerHTML =
        '<p class="decoder__asm">' + hex + "&ensp;&ensp;" + esc(op.asm) + "</p>" +
        '<div class="bits" aria-hidden="true">' + bitsHtml + "</div>" +
        '<div class="bitnums" aria-hidden="true">' + numsHtml + "</div>" +
        '<ul class="fields">' + fieldsHtml + "</ul>" +
        '<p class="decoder__ext">' + esc(op.ext[LANG === "fr" ? 1 : 0]) + "</p>";
    }

    function hot(fi, on) {
      $$('[data-f="' + fi + '"]', panel).forEach(function (el) { el.classList.toggle("is-hot", on); });
    }
    panel.addEventListener("mouseover", function (e) {
      var t = e.target.closest && e.target.closest("[data-f]");
      $$(".is-hot", panel).forEach(function (el) { el.classList.remove("is-hot"); });
      if (t) hot(t.getAttribute("data-f"), true);
    });
    panel.addEventListener("mouseleave", function () {
      $$(".is-hot", panel).forEach(function (el) { el.classList.remove("is-hot"); });
    });
    select(0, false);
  }

  /* ------------------------------------------------------------------
     5. Build-your-own checklist (state kept in this browser only)
     ------------------------------------------------------------------ */
  var KEY = "gase-checklist";
  var saved = [];
  try { saved = JSON.parse(localStorage.getItem(KEY) || "[]") || []; } catch (e) { saved = []; }
  var checks = $$(".checklist button");
  var meta = document.querySelector("[data-check-meta]");
  function updateMeta() {
    if (!meta) return;
    var n = checks.filter(function (b) { return b.getAttribute("aria-pressed") === "true"; }).length;
    meta.textContent = n + T.done;
  }
  checks.forEach(function (b, i) {
    b.setAttribute("aria-pressed", saved[i] ? "true" : "false");
    b.addEventListener("click", function () {
      var on = b.getAttribute("aria-pressed") !== "true";
      b.setAttribute("aria-pressed", on ? "true" : "false");
      saved[i] = on;
      try { localStorage.setItem(KEY, JSON.stringify(saved)); } catch (e) {}
      updateMeta();
    });
  });
  updateMeta();

  /* ------------------------------------------------------------------
     6. Footer CRAM strip: 16 colours, all valid 9-bit Mega Drive values
     ------------------------------------------------------------------ */
  var cram = document.querySelector(".cram");
  if (cram) {
    var pal = [[0,0,0],[1,1,1],[2,2,2],[4,4,4],[7,7,6],[7,2,0],[7,4,0],[7,6,2],[3,7,4],[1,5,3],[1,4,7],[0,2,5],[5,3,7],[7,5,6],[4,1,1],[2,1,3]];
    pal.forEach(function (c) {
      var s = document.createElement("span");
      var hex = "#" + c.map(function (v) { var x = Math.round(v * 255 / 7).toString(16); return x.length < 2 ? "0" + x : x; }).join("").toUpperCase();
      var word = ((c[2] << 9) | (c[1] << 5) | (c[0] << 1)).toString(16).toUpperCase();
      while (word.length < 3) word = "0" + word;
      s.style.background = hex;
      s.title = "CRAM $" + word + " → " + hex;
      cram.appendChild(s);
    });
  }

  /* ------------------------------------------------------------------
     7. Hero: 68000 hex drifting through a "recompiler" seam into C
     ------------------------------------------------------------------ */
  var canvas = document.querySelector(".hero__canvas");
  if (canvas && canvas.getContext) {
    var ctx = canvas.getContext("2d", { alpha: false });
    var HEX = ("4E75 3080 D041 6100 0010 41F9 00FF 0000 4EB9 0000 1234 51C8 FFFC 23FC 4000 0003 00C0 0004 " +
      "4A79 00C0 0004 66F8 3E3C 00FF 2F08 7000 4E71 0C40 0003 6E12 E548 207B 0006 4ED0 46FC 2700 " +
      "33FC 8F02 00C0 0004 4CDF 7FFF 4E73 1018 B03C 0020 67F4 5340").split(" ");
    // generated code, Rust edition and C first draft interleaved
    var CL = [
      "0x27E => { cx.cycles += 18; let v = cx.r8(0xFF0000); cx.logic(v, 1);",
      "PUSH32(0x00000304); f_00000312(); if (gase_cpu.pc != 0x00000304) goto unwind;",
      "if cx.cond(7 /* eq */) { if !cx.poll(0x27E) { return; } pc = 0x27E; continue; }",
      "if (!gase_cpu.z) { CYC(10); POLL(0x00000248); goto L_000248; }",
      "if !cx.call(f_0002E6, 0x2E6, 0x296) { return; } pc = 0x296;",
      "gase_cpu.pc = POP32(); return;",
      "cx.w32(0xC00004, 0x40000003); cx.cycles += 20;",
      "{ uint32_t v = R32(A(1)); A(1) += 4; W32(A(0), v); op_logic(v, 4); } CYC(20);",
      "let r = cx.add(d, s, 2); cx.w16(ea_d, r);",
      "_ => { cx.pc = pc; return; }  // back to the dispatcher"
    ];
    var COLORS = ["#FF4900", "#2492FF", "#6DFF92", "#FFDB48"];
    var W = 0, H = 0, dpr = 1, cw = 7.8, lh = 20, rows = [], seamX = 0, seamTarget = 0, running = false, raf = 0, t0 = 0;
    var FONT = '500 13px "JetBrains Mono", ui-monospace, Menlo, monospace';

    function rnd(seed) { var x = Math.sin(seed * 127.1) * 43758.5453; return x - Math.floor(x); }

    function buildRows() {
      rows = [];
      var n = Math.ceil(H / lh) + 1;
      var need = Math.ceil(W / cw) * 2 + 40;
      for (var r = 0; r < n; r++) {
        var addr = 0x200 + r * 16, hs = "";
        while (hs.length < need) {
          var a = addr.toString(16).toUpperCase();
          while (a.length < 8) a = "0" + a;
          hs += a + "  ";
          for (var k = 0; k < 6; k++) hs += HEX[Math.floor(rnd(addr + k * 13 + r) * HEX.length)] + " ";
          hs += "   ";
          addr += 12;
        }
        var cs = "";
        var ci = r;
        while (cs.length < need) { cs += CL[ci % CL.length] + "   "; ci += 3; }
        rows.push({ hex: hs, c: cs, speed: 14 + rnd(r + 1) * 22, phase: rnd(r + 7) * 400, accent: r % 9 === 4 });
      }
    }

    function resize() {
      var rect = canvas.getBoundingClientRect();
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      W = Math.max(1, Math.round(rect.width));
      H = Math.max(1, Math.round(rect.height));
      canvas.width = Math.round(W * dpr);
      canvas.height = Math.round(H * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.font = FONT;
      cw = ctx.measureText("0000000000").width / 10 || 7.8;
      lh = W < 600 ? 18 : 20;
      seamX = seamTarget = W * (W < 700 ? 0.5 : 0.56);
      buildRows();
    }

    function seamAt(r, t) {
      return seamX + Math.sin(r * 0.42 + t * 0.9) * 26 + Math.sin(r * 0.11 - t * 0.35) * 40;
    }

    function frame(now) {
      if (!t0) t0 = now;
      var t = (now - t0) / 1000;
      seamX += (seamTarget - seamX) * 0.06;
      ctx.fillStyle = "#0a0a0c";
      ctx.fillRect(0, 0, W, H);
      ctx.font = FONT;
      ctx.textBaseline = "top";
      var r, row, sx, off, y;

      // left of the seam: raw ROM words
      ctx.save();
      ctx.beginPath();
      for (r = 0; r < rows.length; r++) ctx.rect(0, r * lh, Math.max(0, seamAt(r, t)), lh);
      ctx.clip();
      ctx.fillStyle = "rgba(241,237,228,0.16)";
      for (r = 0; r < rows.length; r++) {
        row = rows[r];
        off = ((t * row.speed + row.phase) % (cw * 40));
        ctx.fillText(row.hex, off - cw * 40, r * lh + 3);
      }
      ctx.restore();

      // right of the seam: generated C
      ctx.save();
      ctx.beginPath();
      for (r = 0; r < rows.length; r++) { sx = seamAt(r, t); ctx.rect(sx, r * lh, W - sx, lh); }
      ctx.clip();
      for (r = 0; r < rows.length; r++) {
        row = rows[r];
        off = ((t * row.speed + row.phase) % (cw * 40));
        ctx.fillStyle = row.accent ? "rgba(255,73,0,0.62)" : "rgba(241,237,228,0.34)";
        ctx.fillText(row.c, off - cw * 40, r * lh + 3);
      }
      ctx.restore();

      // the seam itself: stepped "pixels" at 10 Hz, very 16-bit
      var step = Math.floor(t * 10);
      var px = W < 600 ? 5 : 6;
      for (r = 0; r < rows.length; r++) {
        sx = seamAt(r, t);
        y = r * lh;
        for (var k = 0; k < 4; k++) {
          var q = rnd(r * 31 + k * 7 + step);
          if (q < 0.35) continue;
          ctx.globalAlpha = 0.25 + q * 0.6;
          ctx.fillStyle = COLORS[(r + k + step) & 3];
          ctx.fillRect(Math.round(sx - 18 + rnd(r + k * 3 + step * 0.5) * 36), y + Math.round(rnd(k + r * 5 + step) * (lh - px)), px, px);
        }
      }
      ctx.globalAlpha = 1;
      if (running) raf = requestAnimationFrame(frame);
    }

    function start() { if (!running && !REDUCED) { running = true; raf = requestAnimationFrame(frame); } }
    function stop() { running = false; cancelAnimationFrame(raf); }

    resize();
    frame(performance.now());
    if (document.fonts && document.fonts.load) {
      document.fonts.load(FONT).then(function () { resize(); frame(performance.now()); }, function () {});
    }

    var visible = true;
    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (en) {
        visible = en[0].isIntersecting;
        if (visible && !document.hidden) start(); else stop();
      }).observe(canvas);
    }
    document.addEventListener("visibilitychange", function () {
      if (document.hidden) stop(); else if (visible) start();
    });
    var rz = 0;
    window.addEventListener("resize", function () {
      clearTimeout(rz);
      rz = setTimeout(function () { resize(); if (!running) frame(performance.now()); }, 120);
    });
    canvas.parentNode.addEventListener("pointermove", function (e) {
      if (e.pointerType !== "mouse") return;
      seamTarget = W * 0.56 + (e.clientX - W / 2) * 0.18;
    });
    if (reduceMQ.addEventListener) {
      reduceMQ.addEventListener("change", function (e) { REDUCED = e.matches; if (REDUCED) stop(); else if (visible) start(); });
    }
    start();
  }
})();
