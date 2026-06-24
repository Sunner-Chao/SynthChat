const ui = {
    bubble: document.getElementById("bubble"),
    bubbleContainer: document.getElementById("bubble-container"),
    thinkingIndicator: document.getElementById("thinking-indicator"),
    status: document.getElementById("pet-status"),
    chatInput: document.getElementById("chat-input"),
    sendBtn: document.getElementById("send-btn")
};

const app = new PIXI.Application({
    view: document.getElementById("canvas"),
    autoStart: true,
    resizeTo: window,
    transparent: true,
    backgroundAlpha: 0,
});

try {
    app.renderer.background.alpha = 0;
} catch {
    // Older Pixi builds may not expose renderer.background.
}

const HOST_MESSAGE_SOURCE = "synthchat-pet-host";
const FRAME_MESSAGE_SOURCE = "synthchat-pet-frame";

let model = null;
let modelNaturalSize = null;
let modelScale = null;
let loadToken = 0;
let hoveringModel = false;
let modelDragTimer = null;
let modelDragPending = false;
let activePointerId = null;
let draggingModel = false;
let dragStartScreenX = 0;
let dragStartScreenY = 0;
let tapTimer = null;

const MODEL_HIT_PADDING = 28;
const MODEL_DRAG_DELAY_MS = 240;
const MODEL_TAP_DELAY_MS = 340;
const DEFAULT_MODEL_URL = "/pet/model/Mao/Mao.model3.json";

let loadingModelKey = null;
let loadedModelUrl = null;

function postMessageToHost(data) {
    const payload = { source: FRAME_MESSAGE_SOURCE, ...data };
    if (window.chrome?.webview?.postMessage) {
        window.chrome.webview.postMessage(payload);
    }
    window.parent?.postMessage(payload, "*");
}

function listenHostMessages(handler) {
    if (window.chrome?.webview?.addEventListener) {
        window.chrome.webview.addEventListener("message", (event) => handler(event.data));
    }
    window.addEventListener("message", (event) => handler(event.data));
}

function showBubble(text) {
    ui.bubble.innerText = text;
    ui.bubbleContainer.classList.add("show");
}

function setStatus(text) {
    if (!ui.status) return;
    const nextText = typeof text === "string" ? text.trim() : "";
    ui.status.innerText = nextText;
    ui.status.classList.toggle("show", Boolean(nextText));
}

function clearModelDragTimer() {
    modelDragPending = false;
    if (modelDragTimer !== null) {
        window.clearTimeout(modelDragTimer);
        modelDragTimer = null;
    }
}

function pointInModelBounds(clientX, clientY) {
    if (!model) return false;
    const bounds = model.getBounds();
    return (
        clientX >= bounds.x - MODEL_HIT_PADDING &&
        clientX <= bounds.x + bounds.width + MODEL_HIT_PADDING &&
        clientY >= bounds.y - MODEL_HIT_PADDING &&
        clientY <= bounds.y + bounds.height + MODEL_HIT_PADDING
    );
}

function setModelHover(nextHovering) {
    if (nextHovering === hoveringModel) return;
    hoveringModel = nextHovering;
    document.body.classList.toggle("model-hover", hoveringModel);
    const bounds = model ? model.getBounds() : null;
    postMessageToHost({
        type: "model_hover",
        hovering: hoveringModel,
        x: bounds?.x,
        y: bounds?.y,
        width: bounds?.width,
        height: bounds?.height
    });
}

function reportModelBounds() {
    if (!model) return;
    const bounds = model.getBounds();
    postMessageToHost({
        type: "model_bounds",
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height
    });
}

function focusScreenPoint(clientX, clientY, instant = false) {
    if (!model) return;
    model.focus(clientX, clientY, instant);
}

function layoutModel() {
    if (!model || !modelNaturalSize || modelScale === null) return;
    model.scale.set(modelScale);
    model.anchor.set(0.5, 0.5);
    model.position.set(window.innerWidth * 0.5, window.innerHeight * 0.56);
    reportModelBounds();
}

function finishModelDrag() {
    const wasDragging = draggingModel;
    activePointerId = null;
    draggingModel = false;
    clearModelDragTimer();
    if (wasDragging) {
        postMessageToHost({ type: "model_drag_end" });
    }
}

function normalizeModelUrl(url) {
    const rawUrl = typeof url === "string" && url.trim() ? url.trim() : DEFAULT_MODEL_URL;
    return rawUrl;
}

function addModelUrlCandidate(candidates, url) {
    if (url && !candidates.includes(url)) {
        candidates.push(url);
    }
}

function modelUrlCandidates(url) {
    const rawUrl = normalizeModelUrl(url);
    const candidates = [];
    addModelUrlCandidate(candidates, rawUrl);
    if (rawUrl.startsWith("/pet/model/")) {
        addModelUrlCandidate(candidates, rawUrl.slice("/pet/".length));
    } else if (rawUrl.startsWith("./model/")) {
        addModelUrlCandidate(candidates, `/pet/${rawUrl.slice(2)}`);
        addModelUrlCandidate(candidates, rawUrl.slice(2));
    } else if (rawUrl.startsWith("model/")) {
        addModelUrlCandidate(candidates, `./${rawUrl}`);
        addModelUrlCandidate(candidates, `/pet/${rawUrl}`);
    }
    return candidates;
}

function errorMessage(error) {
    if (error instanceof Error) return error.message;
    return String(error);
}

async function loadModel(url = DEFAULT_MODEL_URL) {
    const candidates = modelUrlCandidates(url);
    const loadingKey = candidates.join("|");
    if (model && loadedModelUrl && candidates.includes(loadedModelUrl)) {
        postMessageToHost({ type: "loaded", url: loadedModelUrl });
        return;
    }
    if (loadingModelKey === loadingKey) return;
    const currentToken = ++loadToken;
    loadingModelKey = loadingKey;
    setStatus("加载中");
    try {
        if (!PIXI?.live2d?.Live2DModel) {
            throw new Error("Live2D runtime is not ready");
        }
        if (model) {
            app.stage.removeChild(model);
            model.destroy?.({ children: true });
            model = null;
            modelNaturalSize = null;
            modelScale = null;
            loadedModelUrl = null;
        }

        let lastError = null;
        for (const modelUrl of candidates) {
            try {
                const nextModel = await PIXI.live2d.Live2DModel.from(modelUrl, { autoInteract: false });
                if (currentToken !== loadToken) {
                    nextModel.destroy?.({ children: true });
                    return;
                }
                loadingModelKey = null;
                loadedModelUrl = modelUrl;
                model = nextModel;
                modelNaturalSize = {
                    width: Math.max(1, nextModel.width),
                    height: Math.max(1, nextModel.height)
                };
                modelScale = Math.min(
                    (window.innerHeight * 0.82) / modelNaturalSize.height,
                    (window.innerWidth * 0.88) / modelNaturalSize.width
                );
                app.stage.addChild(model);

                const ctrl = model.internalModel.focusController;
                if (ctrl) {
                    ctrl.acceleration = 0.04;
                    ctrl.deceleration = 0.08;
                }

                layoutModel();
                model.interactive = true;
                reportModelBounds();

                setStatus("");
                postMessageToHost({ type: "loaded", url: modelUrl });
                return;
            } catch (error) {
                lastError = error;
                console.warn("Live2D model candidate failed:", modelUrl, error);
            }
        }
        throw lastError ?? new Error("Live2D model load failed");
    } catch (error) {
        if (currentToken === loadToken) {
            loadingModelKey = null;
            loadedModelUrl = null;
        }
        setStatus("模型加载失败");
        postMessageToHost({ type: "error", message: errorMessage(error) });
        console.error(error);
    }
}

listenHostMessages((msg) => {
    if (!msg || typeof msg !== "object") return;
    if (msg.source !== HOST_MESSAGE_SOURCE) return;
    switch (msg.type) {
        case "load":
            void loadModel(msg.url);
            break;
        case "expression":
            try {
                model?.expression(msg.id);
            } catch (error) {
                console.error(error);
            }
            break;
        case "motion":
            try {
                model?.motion(msg.group, msg.index, PIXI.live2d.MotionPriority.FORCE);
            } catch (error) {
                console.error(error);
            }
            break;
        case "bubble":
            showBubble(msg.text);
            break;
        case "hide-bubble":
            ui.bubbleContainer.classList.remove("show");
            break;
        case "look":
            if (typeof msg.x === "number" && typeof msg.y === "number") {
                if (typeof msg.clientX === "number" && typeof msg.clientY === "number") {
                    focusScreenPoint(msg.clientX, msg.clientY, Boolean(msg.instant));
                } else {
                    focusScreenPoint(msg.x, msg.y, Boolean(msg.instant));
                }
            }
            break;
        case "status":
            ui.thinkingIndicator.classList.toggle("show", Boolean(msg.working));
            break;
    }
});

const canvas = document.getElementById("canvas");

canvas.addEventListener("contextmenu", (event) => {
    if (!pointInModelBounds(event.clientX, event.clientY)) return;
    event.preventDefault();
    clearModelDragTimer();
    finishModelDrag();
    postMessageToHost({ type: "model_context_menu", areas: ["model"] });
});

canvas.addEventListener("dblclick", (event) => {
    clearModelDragTimer();
    if (!pointInModelBounds(event.clientX, event.clientY)) return;
    if (tapTimer !== null) {
        window.clearTimeout(tapTimer);
        tapTimer = null;
    }
    postMessageToHost({ type: "poke", areas: ["model"] });
});

canvas.addEventListener("pointermove", (event) => {
    const overModel = pointInModelBounds(event.clientX, event.clientY);
    setModelHover(overModel);
    if (draggingModel && activePointerId === event.pointerId) {
        postMessageToHost({
            type: "model_drag_move",
            screenX: event.screenX,
            screenY: event.screenY
        });
    }
});

canvas.addEventListener("pointerleave", () => {
    if (draggingModel) return;
    setModelHover(false);
});

canvas.addEventListener("pointerdown", (event) => {
    if (event.button !== 0 || !pointInModelBounds(event.clientX, event.clientY)) return;
    event.preventDefault();
    activePointerId = event.pointerId;
    dragStartScreenX = event.screenX;
    dragStartScreenY = event.screenY;
    canvas.setPointerCapture?.(event.pointerId);
    setModelHover(true);
    clearModelDragTimer();
    modelDragPending = true;
    modelDragTimer = window.setTimeout(() => {
        if (!modelDragPending || activePointerId !== event.pointerId) return;
        modelDragPending = false;
        draggingModel = true;
        postMessageToHost({
            type: "model_drag_start",
            screenX: dragStartScreenX,
            screenY: dragStartScreenY
        });
    }, MODEL_DRAG_DELAY_MS);
});

canvas.addEventListener("pointerup", (event) => {
    if (activePointerId !== null && activePointerId !== event.pointerId) return;
    canvas.releasePointerCapture?.(event.pointerId);
    const wasPendingTap = modelDragPending && !draggingModel && pointInModelBounds(event.clientX, event.clientY);
    finishModelDrag();
    if (wasPendingTap) {
        if (tapTimer !== null) window.clearTimeout(tapTimer);
        tapTimer = window.setTimeout(() => {
            tapTimer = null;
            postMessageToHost({ type: "tap", areas: ["model"] });
        }, MODEL_TAP_DELAY_MS);
    }
});
canvas.addEventListener("pointercancel", finishModelDrag);
canvas.addEventListener("lostpointercapture", finishModelDrag);
window.addEventListener("blur", finishModelDrag);
window.addEventListener("resize", layoutModel);

const onSend = () => {
    const text = ui.chatInput.value.trim();
    if (text) {
        postMessageToHost({ type: "input", text });
        ui.chatInput.value = "";
    }
};

ui.sendBtn.onclick = onSend;
ui.chatInput.onkeydown = (event) => {
    if (event.key === "Enter") onSend();
};

postMessageToHost({ type: "ready" });
