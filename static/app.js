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

  var now = document.querySelector(".now-divider");
  if (now && now.scrollIntoView) {
    var rect = now.getBoundingClientRect();
    if (rect.top > innerHeight * 0.7) {
      now.scrollIntoView({ block: "center" });
    }
  }
})();
