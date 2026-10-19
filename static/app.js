(function () {
  try {
    var tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    document.cookie = "tz=" + tz + "; Path=/; SameSite=Lax; Max-Age=31536000";
    document.querySelectorAll('input[type="hidden"][name="timezone"]').forEach(function (el) {
      el.value = tz;
    });
  } catch (e) {}

  var toast = function (message) {
    var region = document.getElementById("toast-region");
    if (!region) return;
    var el = document.createElement("div");
    el.className = "toast toast-error";
    el.setAttribute("role", "alert");
    el.textContent = message;
    region.appendChild(el);
    setTimeout(function () {
      el.remove();
    }, 6000);
  };

  document.body.addEventListener("htmx:responseError", function (ev) {
    var status = ev.detail.xhr ? ev.detail.xhr.status : 0;
    toast(status === 413 ? "That file is too large." : status === 422 ? "Please check the form and try again." : "Something went wrong. Please try again.");
  });
  document.body.addEventListener("htmx:sendError", function () {
    toast("Connection lost. Please try again.");
  });
  document.body.addEventListener("htmx:afterSwap", function (ev) {
    if (ev.target && ev.target.id === "todo-region") {
      var input = document.getElementById("todo-title");
      if (input) input.focus();
    }
  });

  document.addEventListener("click", function (ev) {
    var t = ev.target;
    if (!t.closest) return;
    document.querySelectorAll("details.menu[open]").forEach(function (menu) {
      if (!menu.contains(t)) menu.removeAttribute("open");
    });
    var closer = t.closest("[data-toast-close]");
    if (closer) {
      var el = closer.closest(".toast");
      if (el) el.remove();
      return;
    }
    var clear = t.closest("[data-clear]");
    if (clear) {
      var ids = clear.getAttribute("data-clear").split(" ");
      ids.forEach(function (id, index) {
        var target = document.getElementById(id);
        if (!target) return;
        target.value = "";
        if (index === 0) target.focus();
      });
      return;
    }
    var btn = t.closest("button[data-preset]");
    var form = btn && btn.closest("form");
    if (!form) return;
    var names = "monday tuesday wednesday thursday friday saturday sunday".split(" ");
    var masks = { weekdays: 31, weekends: 96, all: 127 };
    var mask = masks[btn.dataset.preset] || 0;
    names.forEach(function (n, i) {
      var box = form.elements[n];
      if (box) box.checked = ((mask >> i) & 1) === 1;
    });
  });

  document.addEventListener("keydown", function (ev) {
    if (ev.key !== "Escape") return;
    document.querySelectorAll("details.menu[open]").forEach(function (menu) {
      menu.removeAttribute("open");
      var summary = menu.querySelector("summary");
      if (summary) summary.focus();
    });
  });

  document.addEventListener("submit", function (ev) {
    var message = ev.target.getAttribute && ev.target.getAttribute("data-confirm");
    if (message && !confirm(message)) ev.preventDefault();
  });

  document.addEventListener("change", function (ev) {
    var t = ev.target;
    if (t && t.hasAttribute && t.hasAttribute("data-autosubmit") && t.value) {
      t.form.submit();
    }
  });

  var UNITS_WITH_TIME = { days: true, weeks: true };

  var editorTimed = function (editor) {
    var source = document.getElementById(editor.getAttribute("data-time-source"));
    return source ? source.value !== "" : editor.hasAttribute("data-timed");
  };

  var rowNeedsTime = function (row, timed) {
    var unit = row.querySelector("[data-r-unit]").value;
    return !timed || UNITS_WITH_TIME[unit] === true;
  };

  var syncEditor = function (editor) {
    var timed = editorTimed(editor);
    var rows = editor.querySelectorAll("[data-reminder-rows] [data-reminder-row]");
    var specs = [];
    rows.forEach(function (row) {
      var needs = rowNeedsTime(row, timed);
      var wrap = row.querySelector("[data-r-at-wrap]");
      var at = row.querySelector("[data-r-at]");
      wrap.hidden = !needs;
      if (needs && !at.value) at.value = "09:00";
      specs.push({
        offset_value: parseInt(row.querySelector("[data-r-value]").value, 10) || 0,
        offset_unit: row.querySelector("[data-r-unit]").value,
        at_time: needs ? at.value || null : null,
      });
    });
    editor.querySelector("[data-reminders-json]").value = JSON.stringify(specs);
    var empty = editor.querySelector("[data-reminder-empty]");
    if (empty) empty.hidden = rows.length > 0;
    var add = editor.querySelector("[data-reminder-add]");
    if (add) add.disabled = rows.length >= parseInt(editor.getAttribute("data-max"), 10);
  };

  var addRow = function (editor, spec) {
    var template = editor.querySelector("[data-reminder-template]");
    var container = editor.querySelector("[data-reminder-rows]");
    var row = template.content.firstElementChild.cloneNode(true);
    if (spec) {
      row.querySelector("[data-r-value]").value = String(spec.offset_value);
      row.querySelector("[data-r-unit]").value = spec.offset_unit;
      row.querySelector("[data-r-at]").value = spec.at_time || "";
    }
    container.appendChild(row);
  };

  var resetToDefaults = function (editor, timed) {
    var raw = editor.getAttribute(timed ? "data-defaults-timed" : "data-defaults-untimed");
    if (raw === null) return;
    var specs;
    try {
      specs = JSON.parse(raw);
    } catch (e) {
      return;
    }
    editor.querySelector("[data-reminder-rows]").textContent = "";
    specs.forEach(function (spec) {
      addRow(editor, spec);
    });
  };

  var refreshEditors = function (sourceId) {
    document.querySelectorAll("[data-reminders]").forEach(function (editor) {
      if (editor.getAttribute("data-time-source") !== sourceId) return;
      if (!editor.hasAttribute("data-touched")) resetToDefaults(editor, editorTimed(editor));
      syncEditor(editor);
    });
  };

  document.addEventListener("input", function (ev) {
    var t = ev.target;
    if (!t || !t.closest) return;
    var editor = t.closest("[data-reminders]");
    if (editor) {
      editor.setAttribute("data-touched", "");
      syncEditor(editor);
      return;
    }
    if (t.id) refreshEditors(t.id);
  });

  document.addEventListener("change", function (ev) {
    var t = ev.target;
    if (!t || !t.closest) return;
    var editor = t.closest("[data-reminders]");
    if (editor) {
      editor.setAttribute("data-touched", "");
      syncEditor(editor);
      return;
    }
    if (t.id) refreshEditors(t.id);
  });

  document.addEventListener("click", function (ev) {
    var t = ev.target;
    if (!t.closest) return;
    var editor = t.closest("[data-reminders]");
    if (!editor) return;
    var remove = t.closest("[data-reminder-remove]");
    var add = t.closest("[data-reminder-add]");
    if (!remove && !add) return;
    editor.setAttribute("data-touched", "");
    if (remove) {
      var row = remove.closest("[data-reminder-row]");
      if (row) row.remove();
    } else {
      addRow(editor, null);
    }
    syncEditor(editor);
  });

  document.querySelectorAll("[data-reminders]").forEach(syncEditor);
  document.body.addEventListener("htmx:afterSwap", function () {
    document.querySelectorAll("[data-reminders]").forEach(syncEditor);
  });

  var now = document.querySelector(".now-divider");
  if (now && now.scrollIntoView) {
    var rect = now.getBoundingClientRect();
    if (rect.top > innerHeight * 0.7) {
      now.scrollIntoView({ block: "center" });
    }
  }
})();
