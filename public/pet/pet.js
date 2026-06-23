const ui = {
    bubble: document.getElementById("bubble"),
    bubbleContainer: document.getElementById("bubble-container"),
    thinkingIndicator: document.getElementById("thinking-indicator"),
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
const MODEL_TAP_DELAY_MS = 220;
const MODEL_VIEWPORT_WIDTH = 360;

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
    postMessageToHost({ type: "model_hover", hovering: hoveringModel });
}

function focusScreenPoint(clientX, clientY, instant = false) {
    if (!model) return;
    model.focus(clientX, clientY, instant);
}

function layoutModel() {
    if (!model || !modelNaturalSize || modelScale === null) return;
    const modelAreaWidth = Math.min(window.innerWidth, MODEL_VIEWPORT_WIDTH);
    model.scale.set(modelScale);
    model.anchor.set(0.5, 0.5);
    model.position.set(modelAreaWidth * 0.5, window.innerHeight * 0.57);
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

async function loadModel(url) {
    const currentToken = ++loadToken;
    try {
        if (model) {
            app.stage.removeChild(model);
            model.destroy?.({ children: true });
            model = null;
            modelNaturalSize = null;
            modelScale = null;
        }
        const nextModel = await PIXI.live2d.Live2DModel.from(url, { autoInteract: false });
        if (currentToken !== loadToken) {
            nextModel.destroy?.({ children: true });
            return;
        }
        model = nextModel;
        modelNaturalSize = {
            width: Math.max(1, nextModel.width),
            height: Math.max(1, nextModel.height)
        };
        modelScale = Math.min(
            (window.innerHeight * 0.82) / modelNaturalSize.height,
            (MODEL_VIEWPORT_WIDTH * 0.86) / modelNaturalSize.width
        );
        app.stage.addChild(model);

        const ctrl = model.internalModel.focusController;
        if (ctrl) {
            ctrl.acceleration = 0.04;
            ctrl.deceleration = 0.08;
        }

        layoutModel();
        model.interactive = true;

        postMessageToHost({ type: "loaded" });
    } catch (error) {
        postMessageToHost({ type: "error", message: String(error) });
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
