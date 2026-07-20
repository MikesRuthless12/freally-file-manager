/*
 * Freally unified documentation search — a tiny, dependency-free client
 * widget that reads the shared `window.FREALLY_DOCS_INDEX` (search-index.js)
 * and lets a reader find any topic across the WHOLE Freally app family from
 * one box, with live autocomplete suggestions and full keyboard control.
 *
 * Drop-in: include search-index.js then search.js, and put a
 *   <div id="doc-search"></div>
 * where the box should render. The same two files ship in every app's docs.
 */
(function () {
  "use strict";

  var DATA = window.FREALLY_DOCS_INDEX;
  var mount = document.getElementById("doc-search");
  if (!DATA || !mount || !Array.isArray(DATA.entries)) return;

  var ENTRIES = DATA.entries;
  var APPS = DATA.apps || [];

  // Best-effort "which app am I on?" so same-app hits rank first. Matches the
  // repo slug in each app URL against the current location.
  var HERE = (location.href || "").toLowerCase();
  var currentSlug = null;
  for (var i = 0; i < APPS.length; i++) {
    var slug = slugOf(APPS[i].url);
    if (slug && HERE.indexOf("/" + slug + "/") !== -1) {
      currentSlug = slug;
      break;
    }
  }

  function slugOf(url) {
    var m = /github\.io\/([^/]+)\//.exec(url || "");
    return m ? m[1].toLowerCase() : null;
  }

  // ---- Build the DOM -------------------------------------------------------
  mount.innerHTML =
    '<div class="ds-box">' +
    '<span class="ds-icon" aria-hidden="true">⌕</span>' +
    '<input id="doc-search-input" type="search" autocomplete="off" ' +
    'placeholder="Search all Freally apps…" ' +
    'aria-label="Search the documentation" role="combobox" ' +
    'aria-expanded="false" aria-controls="doc-search-results" aria-autocomplete="list" />' +
    "</div>" +
    '<ul id="doc-search-results" class="ds-results" role="listbox" hidden></ul>';

  var input = document.getElementById("doc-search-input");
  var list = document.getElementById("doc-search-results");
  var results = [];
  var active = -1;

  // ---- Ranking -------------------------------------------------------------
  function search(query) {
    var terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return [];
    var scored = [];
    for (var i = 0; i < ENTRIES.length; i++) {
      var e = ENTRIES[i];
      var title = e.title.toLowerCase();
      var hay =
        title + "  " + e.app.toLowerCase() + " " + (e.desc || "").toLowerCase() + " " + (e.keywords || "").toLowerCase();
      var s = 0;
      var ok = true;
      for (var t = 0; t < terms.length; t++) {
        var term = terms[t];
        if (hay.indexOf(term) === -1) {
          ok = false;
          break;
        }
        if (title.indexOf(term) === 0) s += 8;
        else if (title.indexOf(term) !== -1) s += 5;
        else s += 1;
      }
      if (!ok) continue;
      if (currentSlug && slugOf(e.url) === currentSlug) s += 3; // prefer this app
      scored.push({ e: e, s: s });
    }
    scored.sort(function (a, b) {
      return b.s - a.s || a.e.title.localeCompare(b.e.title);
    });
    return scored.slice(0, 8).map(function (x) {
      return x.e;
    });
  }

  function esc(str) {
    return String(str).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }

  function render() {
    if (!results.length) {
      list.innerHTML =
        input.value.trim().length > 0
          ? '<li class="ds-empty" role="option">No matches — try another word.</li>'
          : "";
      list.hidden = !input.value.trim().length;
      input.setAttribute("aria-expanded", list.hidden ? "false" : "true");
      return;
    }
    var html = "";
    for (var i = 0; i < results.length; i++) {
      var r = results[i];
      html +=
        '<li class="ds-item' +
        (i === active ? " ds-active" : "") +
        '" role="option" id="ds-opt-' +
        i +
        '" data-url="' +
        esc(r.url) +
        '"' +
        (i === active ? ' aria-selected="true"' : "") +
        ">" +
        '<span class="ds-app">' +
        esc(r.app.replace(/^Freally /, "")) +
        "</span>" +
        '<span class="ds-text"><span class="ds-title">' +
        esc(r.title) +
        '</span><span class="ds-desc">' +
        esc(r.desc || "") +
        "</span></span>" +
        "</li>";
    }
    list.innerHTML = html;
    list.hidden = false;
    input.setAttribute("aria-expanded", "true");
  }

  function go(entry) {
    if (entry && entry.url) window.location.href = entry.url;
  }

  // ---- Events --------------------------------------------------------------
  input.addEventListener("input", function () {
    results = search(input.value);
    active = -1;
    render();
  });

  input.addEventListener("keydown", function (ev) {
    if (list.hidden || !results.length) {
      if (ev.key === "Enter" && results.length) go(results[0]);
      return;
    }
    if (ev.key === "ArrowDown") {
      ev.preventDefault();
      active = (active + 1) % results.length;
      updateActive();
    } else if (ev.key === "ArrowUp") {
      ev.preventDefault();
      active = (active - 1 + results.length) % results.length;
      updateActive();
    } else if (ev.key === "Enter") {
      ev.preventDefault();
      go(results[active >= 0 ? active : 0]);
    } else if (ev.key === "Escape") {
      list.hidden = true;
      input.setAttribute("aria-expanded", "false");
    }
  });

  function updateActive() {
    var items = list.querySelectorAll(".ds-item");
    for (var i = 0; i < items.length; i++) {
      var on = i === active;
      items[i].classList.toggle("ds-active", on);
      if (on) {
        items[i].setAttribute("aria-selected", "true");
        input.setAttribute("aria-activedescendant", items[i].id);
        items[i].scrollIntoView({ block: "nearest" });
      } else {
        items[i].removeAttribute("aria-selected");
      }
    }
  }

  list.addEventListener("mousedown", function (ev) {
    var li = ev.target.closest(".ds-item");
    if (li && li.dataset.url) {
      ev.preventDefault();
      window.location.href = li.dataset.url;
    }
  });

  // Close when focus leaves the widget.
  document.addEventListener("click", function (ev) {
    if (!mount.contains(ev.target)) {
      list.hidden = true;
      input.setAttribute("aria-expanded", "false");
    }
  });
})();
