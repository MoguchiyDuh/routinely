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
        target.dispatchEvent(new Event("input", { bubbles: true }));
        target.dispatchEvent(new Event("change", { bubbles: true }));
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

  var syncNeedsValue = function () {
    document.querySelectorAll("[data-needs-value]").forEach(function (el) {
      var source = document.getElementById(el.getAttribute("data-needs-value"));
      el.hidden = !source || source.value === "";
    });
  };

  document.querySelectorAll("[data-reminders]").forEach(syncEditor);
  document.body.addEventListener("htmx:afterSwap", function () {
    document.querySelectorAll("[data-reminders]").forEach(syncEditor);
    syncNeedsValue();
  });
  document.addEventListener("input", syncNeedsValue);
  document.addEventListener("change", syncNeedsValue);

  var PHOTO_TYPES = { "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp" };
  var MAX_PHOTO_BYTES = 10 * 1024 * 1024;
  var canSetFiles = (function () {
    try {
      new DataTransfer();
      return true;
    } catch (e) {
      return false;
    }
  })();
  var isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || "");
  var pasteCounter = 0;

  var toArray = function (list) {
    return Array.prototype.slice.call(list || []);
  };

  var photoField = function (el) {
    return el && el.closest ? el.closest("[data-photo-field]") : null;
  };

  var renderPreviews = function (input) {
    var field = photoField(input);
    var list = field && field.querySelector("[data-photo-previews]");
    if (!list) return;
    (list._urls || []).forEach(function (url) {
      URL.revokeObjectURL(url);
    });
    list._urls = [];
    list.textContent = "";
    var template = field.querySelector("[data-photo-remove-template]");
    var files = toArray(input.files);
    files.forEach(function (file, index) {
      var li = document.createElement("li");
      li.className = "photo-preview";
      var url = URL.createObjectURL(file);
      list._urls.push(url);
      var img = document.createElement("img");
      img.src = url;
      img.alt = file.name;
      li.appendChild(img);
      if (canSetFiles && template) {
        var remove = template.content.firstElementChild.cloneNode(true);
        remove.setAttribute("data-photo-remove", String(index));
        remove.setAttribute("aria-label", "Remove " + file.name);
        li.appendChild(remove);
      }
      list.appendChild(li);
    });
    list.hidden = files.length === 0;
  };

  var setPhotos = function (input, files) {
    var dt = new DataTransfer();
    files.forEach(function (file) {
      dt.items.add(file);
    });
    input.files = dt.files;
    input._photos = files;
    renderPreviews(input);
  };

  var addPhotos = function (input, files) {
    var current = input._photos || toArray(input.files);
    var added = 0;
    files.forEach(function (file) {
      var ext = PHOTO_TYPES[file.type];
      if (!ext) {
        toast("Only PNG, JPEG or WebP images can be attached.");
        return;
      }
      if (file.size > MAX_PHOTO_BYTES) {
        toast("That image is larger than 10 MB.");
        return;
      }
      pasteCounter += 1;
      var name = "pasted-" + Date.now() + "-" + pasteCounter + "." + ext;
      current.push(new File([file], name, { type: file.type }));
      added += 1;
    });
    if (added) setPhotos(input, current);
    return added;
  };

  var pasteTarget = function (target) {
    var scope = target && target.closest ? target.closest("details.proof[open]") : null;
    if (scope) return scope.querySelector("[data-photo-input]");
    if (target && target !== document.body && target !== document.documentElement) return null;
    var open = document.querySelectorAll("details.proof[open] [data-photo-input]");
    return open.length === 1 ? open[0] : null;
  };

  var enablePaste = function () {
    if (!canSetFiles) return;
    document.querySelectorAll("[data-paste-hint][hidden]").forEach(function (el) {
      if (isMac) el.textContent = " · or paste with ⌘V";
      el.hidden = false;
    });
  };

  document.addEventListener("paste", function (ev) {
    if (!canSetFiles || !ev.clipboardData) return;
    var input = pasteTarget(ev.target);
    if (!input) return;
    var t = ev.target;
    var inTextField = t && (t.tagName === "TEXTAREA" || (t.tagName === "INPUT" && t.type !== "file"));
    var types = toArray(ev.clipboardData.types);
    if (inTextField && types.indexOf("text/plain") !== -1) return;
    var files = [];
    toArray(ev.clipboardData.items).forEach(function (item) {
      if (item.kind !== "file" || item.type.indexOf("image/") !== 0) return;
      var file = item.getAsFile();
      if (file) files.push(file);
    });
    if (!files.length) return;
    ev.preventDefault();
    addPhotos(input, files);
  });

  document.addEventListener("change", function (ev) {
    var input = ev.target;
    if (!input || !input.hasAttribute || !input.hasAttribute("data-photo-input")) return;
    if (!canSetFiles) {
      renderPreviews(input);
      return;
    }
    setPhotos(input, (input._photos || []).concat(toArray(input.files)));
  });

  document.addEventListener("click", function (ev) {
    var t = ev.target;
    if (!t.closest) return;
    var remove = t.closest("[data-photo-remove]");
    if (!remove) return;
    var field = photoField(t);
    var input = field && field.querySelector("[data-photo-input]");
    if (!input) return;
    var files = toArray(input.files);
    files.splice(parseInt(remove.getAttribute("data-photo-remove"), 10), 1);
    setPhotos(input, files);
    var next = field.querySelector("[data-photo-remove]");
    (next || input).focus();
  });

  enablePaste();
  document.body.addEventListener("htmx:afterSwap", enablePaste);

  var now = document.querySelector(".now-divider");
  if (now && now.scrollIntoView) {
    var rect = now.getBoundingClientRect();
    if (rect.top > innerHeight * 0.7) {
      now.scrollIntoView({ block: "center" });
    }
  }
})();
