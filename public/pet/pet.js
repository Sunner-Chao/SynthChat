const canvas = document.getElementById("canvas");

const app = new PIXI.Application({
    view: canvas,
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
let dragOriginScreenX = 0;
let dragOriginScreenY = 0;
let dragStartScreenX = 0;
let dragStartScreenY = 0;
let tapTimer = null;
let pendingDragMove = null;
let dragMoveFrame = null;
let lastDragScreenX = null;
let lastDragScreenY = null;
let lastDragMoveAt = 0;
let dragPhysics = { x: 0, y: 0 };
let physicsFrame = null;
let releasePhysicsStartedAt = 0;
let behaviorFrame = null;
let behaviorStartedAt = 0;
let activeBehavior = null;
let idleBehaviorTimer = null;
let modelParameterIds = new Set();
let modelExpressionNames = new Set();
let modelMotionGroups = new Map();
let ambientStartedAt = performance.now();
let behaviorDebugSerial = 0;

const MODEL_HIT_PADDING = 28;
const MODEL_DRAG_DELAY_MS = 90;
const MODEL_DRAG_START_MOVE_PX = 3;
const MODEL_TAP_CONFIRM_DELAY_MS = 300;
const MODEL_LAYOUT_BASE_HEIGHT = 440;
const MODEL_VERTICAL_SCALE_RATIO = 0.74;
const MODEL_HORIZONTAL_SCALE_RATIO = 0.84;
const MODEL_VERTICAL_ANCHOR_RATIO = 0.6;
const DEFAULT_MODEL_URL = "/pet/model/Hiyori/Hiyori.model3.json";
const TOUCH_HEAD_RATIO = 0.38;
const DRAG_PARAM_DAMPING = 0.22;
const RELEASE_PHYSICS_MS = 760;
const IDLE_BEHAVIOR_MIN_DELAY_MS = 6500;
const IDLE_BEHAVIOR_JITTER_MS = 8500;
const PARAMETER_OVERLAY_WEIGHT = 0.78;
const AMBIENT_OVERLAY_WEIGHT = 0.26;
const DRAG_OVERLAY_WEIGHT = 0.84;
const ADDITIVE_PARAM_PREFIXES = [
    "ParamAngle",
    "ParamBodyAngle",
    "ParamHair",
    "ParamShoulder",
    "ParamArm",
    "ParamHand",
    "ParamLeg"
];
const ADDITIVE_PARAM_IDS = new Set(["ParamEyeBallX", "ParamEyeBallY"]);

let loadingModelKey = null;
let loadedModelUrl = null;

function postMessageToHost(data) {
    const payload = { source: FRAME_MESSAGE_SOURCE, ...data };
    if (window.chrome?.webview?.postMessage) {
        window.chrome.webview.postMessage(payload);
    }
    window.parent?.postMessage(payload, "*");
}

function clearTapTimer() {
    if (tapTimer !== null) {
        window.clearTimeout(tapTimer);
        tapTimer = null;
    }
}

function scheduleModelTap(clientX, clientY) {
    clearTapTimer();
    tapTimer = window.setTimeout(() => {
        tapTimer = null;
        void (async () => {
            if (!pointInModelBounds(clientX, clientY)) return;
            const touch = await touchInfoAtPoint(clientX, clientY);
            stopBehaviorAnimation();
            playTouchMotion(touch.area);
            playBehavior(touch.area === "head" ? "happy" : "idle", { durationMs: 900 });
            postMessageToHost({
                type: "tap",
                area: touch.area,
                areas: touch.areas.length > 0 ? touch.areas : [touch.area]
            });
        })();
    }, MODEL_TAP_CONFIRM_DELAY_MS);
}

function listenHostMessages(handler) {
    if (window.chrome?.webview?.addEventListener) {
        window.chrome.webview.addEventListener("message", (event) => handler(event.data));
    }
    window.addEventListener("message", (event) => handler(event.data));
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

function setModelPressing(pressing) {
    document.body.classList.toggle("model-pressing", Boolean(pressing));
}

function setModelDragging(dragging) {
    document.body.classList.toggle("model-dragging", Boolean(dragging));
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

function getModelSettings() {
    return model?.internalModel?.settings ?? null;
}

function getModelMotions() {
    const settings = getModelSettings();
    return (
        settings?.FileReferences?.Motions ??
        settings?.motions ??
        settings?.definitions?.FileReferences?.Motions ??
        settings?.json?.FileReferences?.Motions ??
        settings?._json?.FileReferences?.Motions ??
        {}
    );
}

function getModelExpressions() {
    const settings = getModelSettings();
    return (
        settings?.FileReferences?.Expressions ??
        settings?.expressions ??
        settings?.json?.FileReferences?.Expressions ??
        settings?._json?.FileReferences?.Expressions ??
        []
    );
}

function refreshModelCapabilities() {
    const coreModel = model?.internalModel?.coreModel;
    modelParameterIds = new Set();
    modelExpressionNames = new Set();
    modelMotionGroups = new Map();
    const ids = coreModel?._parameterIds ?? coreModel?.getModel?.()?.parameters?.ids;
    if (ids && typeof ids.length === "number") {
        for (let index = 0; index < ids.length; index += 1) {
            const id = ids[index];
            if (typeof id === "string" && id) modelParameterIds.add(id);
        }
    }
    const expressions = getModelExpressions();
    if (Array.isArray(expressions)) {
        for (const expression of expressions) {
            const name = expression?.Name ?? expression?.name;
            if (typeof name === "string" && name.trim()) modelExpressionNames.add(name.trim());
        }
    }
    const motions = getModelMotions();
    for (const [group, entries] of Object.entries(motions)) {
        if (!Array.isArray(entries)) continue;
        modelMotionGroups.set(group, entries);
    }
    postMessageToHost({
        type: "model_capabilities",
        parameterCount: modelParameterIds.size,
        motionGroups: Array.from(modelMotionGroups.entries()).map(([group, entries]) => ({
            group,
            count: entries.length,
            names: entries
                .map((entry) => entry?.Name ?? entry?.name ?? entry?.File ?? entry?.file)
                .filter((name) => typeof name === "string")
                .slice(0, 6)
        })),
        expressions: Array.from(modelExpressionNames),
        sampleParams: Array.from(modelParameterIds).slice(0, 28)
    });
}

function hasLive2DParam(paramId) {
    if (!paramId) return false;
    if (modelParameterIds.size === 0) return true;
    return modelParameterIds.has(paramId);
}

function setLive2DParam(paramId, value, weight = 1) {
    if (!hasLive2DParam(paramId)) return false;
    const coreModel = model?.internalModel?.coreModel;
    if (!coreModel) return false;
    try {
        if (typeof coreModel.setParameterValueById === "function") {
            coreModel.setParameterValueById(paramId, value, weight);
            return true;
        }
        if (typeof coreModel.addParameterValueById === "function") {
            coreModel.addParameterValueById(paramId, value, weight);
            return true;
        }
    } catch {
        return false;
    }
    return false;
}

function addLive2DParam(paramId, value, weight = 1) {
    if (!hasLive2DParam(paramId)) return false;
    const coreModel = model?.internalModel?.coreModel;
    if (!coreModel) return false;
    try {
        if (typeof coreModel.addParameterValueById === "function") {
            coreModel.addParameterValueById(paramId, value, weight);
            return true;
        }
        if (typeof coreModel.getParameterValueById === "function" && typeof coreModel.setParameterValueById === "function") {
            coreModel.setParameterValueById(paramId, coreModel.getParameterValueById(paramId) + value * weight);
            return true;
        }
    } catch {
        return false;
    }
    return false;
}

function isAdditiveParam(paramId) {
    return ADDITIVE_PARAM_IDS.has(paramId) || ADDITIVE_PARAM_PREFIXES.some((prefix) => paramId.startsWith(prefix));
}

function applyParameterPose(params, weight = 0.5) {
    if (!params || typeof params !== "object") return;
    for (const [paramId, value] of Object.entries(params)) {
        if (typeof value === "number" && hasLive2DParam(paramId)) {
            if (isAdditiveParam(paramId)) {
                addLive2DParam(paramId, value, weight);
            } else {
                setLive2DParam(paramId, value, weight);
            }
        }
    }
}

function resetExpressiveParams(weight = 0.3) {
    applyParameterPose({
        ParamBodyAngleX: 0,
        ParamBodyAngleY: 0,
        ParamBodyAngleZ: 0,
        ParamAngleX: 0,
        ParamAngleY: 0,
        ParamAngleZ: 0,
        ParamEyeLOpen: 1,
        ParamEyeROpen: 1,
        ParamMouthOpenY: 0,
        ParamMouthOpen: 0,
        ParamMouthForm: 0,
        ParamHairAhoge: 0,
        ParamHairFront: 0,
        ParamHairSide: 0,
        ParamHairSideL: 0,
        ParamHairSideR: 0,
        ParamHairBack: 0
    }, weight);
}

function mergePose(...poses) {
    const merged = {};
    for (const pose of poses) {
        if (!pose || typeof pose !== "object") continue;
        for (const [key, value] of Object.entries(pose)) {
            if (typeof value === "number" && hasLive2DParam(key)) {
                merged[key] = (merged[key] ?? 0) + value;
            }
        }
    }
    return merged;
}

function mapPoseAliases(pose) {
    const next = { ...pose };
    if (typeof next.ParamMouthOpenY === "number") {
        next.ParamMouthOpen = next.ParamMouthOpenY;
        next.ParamA = Math.max(next.ParamA ?? 0, next.ParamMouthOpenY);
    }
    if (typeof next.ParamMouthForm === "number") {
        next.ParamMouthUp = Math.max(0, next.ParamMouthForm);
        next.ParamMouthDown = Math.max(0, -next.ParamMouthForm);
    }
    if (typeof next.ParamAngleX === "number") {
        next.ParamEyeBallX = next.ParamEyeBallX ?? next.ParamAngleX / 30;
    }
    if (typeof next.ParamAngleY === "number") {
        next.ParamEyeBallY = next.ParamEyeBallY ?? next.ParamAngleY / 30;
    }
    return next;
}

function ambientPose(now = performance.now()) {
    const t = (now - ambientStartedAt) / 1000;
    const breath = (Math.sin(t * 1.35) + 1) / 2;
    const sway = Math.sin(t * 0.72);
    const blinkPhase = (t % 4.8) / 4.8;
    const blink = blinkPhase > 0.94 ? Math.sin((blinkPhase - 0.94) / 0.06 * Math.PI) : 0;
    return {
        ParamBreath: 0.28 + breath * 0.62,
        ParamBodyAngleY: Math.sin(t * 1.05) * 1.7,
        ParamBodyAngleX: sway * 1.15,
        ParamAngleZ: Math.sin(t * 0.58) * 0.9,
        ParamShoulder: Math.sin(t * 1.1) * 0.45,
        ParamEyeLOpen: 1 - blink * 0.82,
        ParamEyeROpen: 1 - blink * 0.82,
        ParamHairAhoge: Math.sin(t * 1.8) * 2.2,
        ParamHairFront: Math.sin(t * 1.6) * 1.6,
        ParamHairSide: Math.sin(t * 1.5) * 1.6,
        ParamHairBack: Math.sin(t * 1.2) * 1.2
    };
}

function currentBehaviorPose(now = performance.now()) {
    if (!activeBehavior) return null;
    const progress = Math.min(1, (now - behaviorStartedAt) / activeBehavior.duration);
    const pose = behaviorPose(activeBehavior.name, progress, activeBehavior.options);
    if (progress >= 1) {
        activeBehavior = null;
    }
    return pose;
}

function currentDragPose(now = performance.now()) {
    if (draggingModel) return dragPhysicsPose(dragPhysics.x, dragPhysics.y);
    if (!releasePhysicsStartedAt || now - releasePhysicsStartedAt > RELEASE_PHYSICS_MS) return null;
    const progress = Math.min(1, (now - releasePhysicsStartedAt) / RELEASE_PHYSICS_MS);
    const envelope = Math.pow(1 - progress, 1.35);
    const bounce = Math.cos(progress * Math.PI * 3.4) * envelope;
    if (progress >= 1) {
        releasePhysicsStartedAt = 0;
        dragPhysics = { x: 0, y: 0 };
        return null;
    }
    return dragPhysicsPose(dragPhysics.x * bounce * 1.18, dragPhysics.y * bounce * 1.08);
}

function applyFrameOverlay() {
    if (!model) return;
    const now = performance.now();
    const pose = mapPoseAliases(mergePose(
        ambientPose(now),
        currentBehaviorPose(now),
        currentDragPose(now)
    ));
    applyParameterPose(pose, PARAMETER_OVERLAY_WEIGHT);
}

function installModelOverlay(nextModel) {
    const internal = nextModel?.internalModel;
    if (!internal || internal.__synthchatOverlayInstalled) return;
    const originalUpdate = internal.update;
    if (typeof originalUpdate !== "function") return;
    internal.update = function patchedSynthChatUpdate(deltaTime, elapsedTime) {
        const result = originalUpdate.call(this, deltaTime, elapsedTime);
        if (model === nextModel && nextModel.internalModel === internal) {
            applyFrameOverlay();
        }
        return result;
    };
    internal.__synthchatOverlayInstalled = true;
}

function dragPhysicsPose(rawX, rawY) {
    const swayX = Math.max(-38, Math.min(38, rawX));
    const swayY = Math.max(-26, Math.min(26, rawY));
    return {
        ParamBodyAngleX: -swayX * 0.78,
        ParamBodyAngleY: swayY * 0.34,
        ParamBodyAngleZ: -swayX * 0.28,
        ParamAngleZ: -swayX * 0.52,
        ParamHairAhoge: -swayX * 1.35,
        ParamHairFront: -swayX * 1.08,
        ParamHairSide: -swayX * 1.12,
        ParamHairSideL: -swayX * 1.12,
        ParamHairSideR: -swayX * 1.12,
        ParamHairBack: -swayX * 0.86,
        ParamShoulder: Math.max(-8, Math.min(8, swayY * 0.35))
    };
}

function applyDragPhysics(rawX, rawY, easing = DRAG_PARAM_DAMPING) {
    dragPhysics.x += (rawX - dragPhysics.x) * easing;
    dragPhysics.y += (rawY - dragPhysics.y) * easing;
}

function stopReleasePhysics() {
    if (physicsFrame !== null) {
        window.cancelAnimationFrame(physicsFrame);
        physicsFrame = null;
    }
    releasePhysicsStartedAt = 0;
}

function startReleasePhysics(seedX = dragPhysics.x, seedY = dragPhysics.y) {
    stopReleasePhysics();
    dragPhysics = { x: seedX, y: seedY };
    releasePhysicsStartedAt = performance.now();
}

function clearIdleBehaviorTimer() {
    if (idleBehaviorTimer !== null) {
        window.clearTimeout(idleBehaviorTimer);
        idleBehaviorTimer = null;
    }
}

function scheduleIdleBehavior() {
    clearIdleBehaviorTimer();
    if (!model) return;
    const delay = IDLE_BEHAVIOR_MIN_DELAY_MS + Math.random() * IDLE_BEHAVIOR_JITTER_MS;
    idleBehaviorTimer = window.setTimeout(() => {
        idleBehaviorTimer = null;
        if (!draggingModel && !modelDragPending) {
            const idleBehaviors = ["idle", "curious", "sleepy", "shy", "listening"];
            playBehavior(idleBehaviors[Math.floor(Math.random() * idleBehaviors.length)], { subtle: true });
        }
        scheduleIdleBehavior();
    }, delay);
}

function normalizeHitAreaName(area) {
    return typeof area === "string" ? area.trim().toLowerCase() : "";
}

function semanticTouchAreaFromPoint(clientX, clientY, nativeAreas = []) {
    const normalizedAreas = nativeAreas.map(normalizeHitAreaName);
    if (normalizedAreas.some((area) => area.includes("head"))) return "head";
    if (!model) return "model";
    const bounds = model.getBounds();
    const relativeY = bounds.height > 0 ? (clientY - bounds.y) / bounds.height : 1;
    const relativeX = bounds.width > 0 ? (clientX - bounds.x) / bounds.width : 0.5;
    const inHeadBand = relativeY >= 0 && relativeY <= TOUCH_HEAD_RATIO + 0.12;
    const nearCenter = relativeX >= 0.22 && relativeX <= 0.78;
    if (inHeadBand && nearCenter) return "head";
    if (normalizedAreas.some((area) => area.includes("body") || area.includes("belly"))) return "body";
    return "body";
}

async function touchInfoAtPoint(clientX, clientY) {
    if (!model || !pointInModelBounds(clientX, clientY)) {
        return { area: "model", areas: [] };
    }
    let nativeAreas = [];
    try {
        const result = await Promise.resolve(model.hitTest?.(clientX, clientY));
        if (Array.isArray(result)) {
            nativeAreas = result.filter((area) => typeof area === "string" && area.trim());
        }
    } catch (error) {
        console.warn("Live2D hitTest failed:", error);
    }
    return {
        area: semanticTouchAreaFromPoint(clientX, clientY, nativeAreas),
        areas: nativeAreas
    };
}

function motionGroupCount(group) {
    const groupMotions = modelMotionGroups.get(group) ?? getModelMotions()?.[group];
    return Array.isArray(groupMotions) ? groupMotions.length : null;
}

function tryPlayMotion(group, index = 0) {
    if (!model || !group) return false;
    const count = motionGroupCount(group);
    if (count !== null && count <= 0) return false;
    const safeIndex = count !== null ? Math.min(index, count - 1) : index;
    try {
        model.motion(group, safeIndex, PIXI.live2d.MotionPriority.FORCE);
        postMessageToHost({ type: "motion_debug", group, index: safeIndex });
        return true;
    } catch (error) {
        console.warn("Live2D motion failed:", group, safeIndex, error);
        return false;
    }
}

function normalizeMotionText(value) {
    return String(value ?? "")
        .toLowerCase()
        .replace(/[\s_\-./\\]+/g, "");
}

function motionEntryText(group, entry, index) {
    return normalizeMotionText([
        group,
        entry?.Name,
        entry?.name,
        entry?.File,
        entry?.file,
        index
    ].filter(Boolean).join(" "));
}

function motionKeywordScore(text, keywords, fallbackScore = 0) {
    let score = fallbackScore;
    for (const keyword of keywords) {
        const normalized = normalizeMotionText(keyword);
        if (normalized && text.includes(normalized)) score += 10;
    }
    return score;
}

function rankedMotionCandidates(keywords, options = {}) {
    const includeIdle = Boolean(options.includeIdle);
    const preferNonIdle = Boolean(options.preferNonIdle);
    const candidates = [];
    for (const [group, entries] of modelMotionGroups.entries()) {
        const groupText = normalizeMotionText(group);
        const idleLike = groupText.includes("idle") || groupText.includes("待机");
        if (!includeIdle && idleLike) continue;
        entries.forEach((entry, index) => {
            const text = motionEntryText(group, entry, index);
            let score = motionKeywordScore(text, keywords, 0);
            if (idleLike) score -= preferNonIdle ? 10 : 2;
            if (!idleLike && preferNonIdle) score += 4;
            if (text.includes("tap") || text.includes("touch") || text.includes("flick") || text.includes("点击") || text.includes("触摸")) score += 8;
            if (text.includes("idle") || text.includes("待机")) score += includeIdle ? 1 : -8;
            candidates.push({ group, index, score });
        });
    }
    candidates.sort((left, right) => right.score - left.score);
    return candidates.map(({ group, index }) => [group, index]);
}

function firstAvailableMotionFromGroups(groups) {
    const candidates = [];
    for (const group of groups) {
        const count = motionGroupCount(group);
        if (count && count > 0) {
            candidates.push([group, Math.floor(Math.random() * count)]);
        }
    }
    return candidates;
}

function motionCandidatesForArea(area) {
    const tapBodyCount = motionGroupCount("TapBody");
    const tapCount = motionGroupCount("Tap");
    if (area === "head") {
        return [
            ["TapHead", 0],
            ["TapBody", tapBodyCount && tapBodyCount > 1 ? 1 : 0],
            ["Tap", 0],
            ...rankedMotionCandidates(["head", "taphead", "touchhead", "flickup", "摸头", "头", "帽檐", "害羞", "开心"], { preferNonIdle: true }),
            ["Idle", 0],
            ...rankedMotionCandidates(["idle", "待机"], { includeIdle: true })
        ];
    }
    return [
        ["TapBody", tapBodyCount && tapBodyCount > 0 ? Math.floor(Math.random() * tapBodyCount) : 0],
        ["Tap", tapCount && tapCount > 0 ? Math.floor(Math.random() * tapCount) : 0],
        ...rankedMotionCandidates(["body", "tapbody", "touch", "tap", "flick", "身体", "摆手", "摇晃"], { preferNonIdle: true }),
        ["Idle", 0],
        ...rankedMotionCandidates(["idle", "待机"], { includeIdle: true })
    ];
}

function playTouchMotion(area) {
    for (const [group, index] of motionCandidatesForArea(area)) {
        if (tryPlayMotion(group, index)) return;
    }
    const fallback = firstAvailableMotionFromGroups(["TapBody", "Tap", "FlickUp", "Flick", "FlickDown", "Idle"]);
    for (const [group, index] of fallback) {
        if (tryPlayMotion(group, index)) return;
    }
}

function randomMotion(group) {
    const count = motionGroupCount(group);
    if (!count || count <= 0) return false;
    return tryPlayMotion(group, Math.floor(Math.random() * count));
}

function pickExpression(candidates) {
    if (!Array.isArray(candidates) || modelExpressionNames.size === 0) return null;
    const available = Array.from(modelExpressionNames).map((name) => [name, name.toLowerCase()]);
    for (const candidate of candidates) {
        const normalized = String(candidate).toLowerCase();
        const match = available.find(([, lowerName]) => lowerName === normalized || lowerName.includes(normalized) || normalized.includes(lowerName));
        if (match) return match[0];
    }
    return null;
}

function tryExpression(candidates) {
    const expression = pickExpression(candidates);
    if (!expression) return false;
    try {
        if (typeof model?.expression === "function") {
            model.expression(expression);
            return true;
        }
        const manager = model?.internalModel?.motionManager?.expressionManager;
        if (typeof manager?.setExpression === "function") {
            void manager.setExpression(expression);
            return true;
        }
        return true;
    } catch (error) {
        console.warn("Live2D expression failed:", expression, error);
        return false;
    }
}

function expressionCandidatesForBehavior(name) {
    if (name === "happy" || name === "proud" || name === "wave") return ["开心", "Smile", "Blushing", "害羞", "繁星眼"];
    if (name === "shy") return ["害羞", "Blushing", "Smile", "委屈"];
    if (name === "error") return ["惊讶", "Surprised", "Sad", "委屈", "生气", "Angry"];
    if (name === "sleepy") return ["闭眼", "Sad", "Normal", "通常"];
    if (name === "surprise") return ["惊讶", "Surprised", "繁星眼"];
    if (name === "curious" || name === "listening") return ["通常", "Normal", "Smile"];
    return ["通常", "Normal"];
}

function motionCandidatesForBehavior(name) {
    const normalized = typeof name === "string" ? name : "idle";
    if (normalized === "thinking") {
        return [["Idle", 0], ...rankedMotionCandidates(["thinking", "idle", "待机", "思考"], { includeIdle: true }), ["TapBody", 0]];
    }
    if (normalized === "happy") {
        return [["TapBody", 0], ["FlickUp", 0], ["Flick", 0], ...rankedMotionCandidates(["happy", "smile", "touch", "开心", "害羞", "摆手"], { preferNonIdle: true }), ["Idle", 0]];
    }
    if (normalized === "wave" || normalized === "proud") {
        return [["TapBody", 2], ["TapBody", 0], ["FlickUp", 0], ...rankedMotionCandidates(["wave", "hello", "greet", "帽檐", "打招呼", "摆手"], { preferNonIdle: true }), ["Idle", 0]];
    }
    if (normalized === "curious" || normalized === "listening" || normalized === "shy") {
        return [["TapBody", 1], ["Idle", 1], ...rankedMotionCandidates(["curious", "listen", "shy", "害羞", "不好意思", "摇晃"], { preferNonIdle: true, includeIdle: true }), ["TapBody", 0], ["Idle", 0]];
    }
    if (normalized === "stretch") {
        return [["Idle", 1], ["TapBody", 1], ["FlickUp", 0], ...rankedMotionCandidates(["stretch", "idle", "待机", "伸展"], { includeIdle: true }), ["Idle", 0]];
    }
    if (normalized === "error") {
        return [["FlickDown", 0], ["TapBody", 0], ["Flick", 0], ...rankedMotionCandidates(["angry", "sad", "surprise", "生气", "惊讶", "委屈"], { preferNonIdle: true }), ["Idle", 0]];
    }
    if (normalized === "sleepy") {
        return [["Idle", 2], ...rankedMotionCandidates(["sleep", "idle", "闭眼", "待机"], { includeIdle: true }), ["Idle", 0], ["TapBody", 0]];
    }
    if (normalized === "surprise") {
        return [["TapBody", 0], ["FlickUp", 0], ...rankedMotionCandidates(["surprise", "flick", "惊讶", "繁星"], { preferNonIdle: true }), ["Idle", 0]];
    }
    return [["Idle", null], ...rankedMotionCandidates(["idle", "touch", "待机"], { includeIdle: true }), ["TapBody", 0]];
}

function playBehaviorMotion(name) {
    for (const [group, index] of motionCandidatesForBehavior(name)) {
        if (index === null) {
            if (randomMotion(group)) return true;
        } else if (tryPlayMotion(group, index)) {
            return true;
        }
    }
    return false;
}

function stopBehaviorAnimation() {
    if (behaviorFrame !== null) {
        window.cancelAnimationFrame(behaviorFrame);
        behaviorFrame = null;
    }
    activeBehavior = null;
}

function behaviorPose(name, progress, options = {}) {
    const wave = Math.sin(progress * Math.PI);
    const doubleWave = Math.sin(progress * Math.PI * 2);
    const smallWave = Math.sin(progress * Math.PI * 4);
    if (name === "thinking") {
        return {
            ParamBodyAngleX: doubleWave * 4.6,
            ParamBodyAngleY: -6.5 * wave,
            ParamAngleX: doubleWave * 5.2,
            ParamAngleY: -9 * wave,
            ParamBrowLY: -0.52 * wave,
            ParamBrowRY: -0.52 * wave,
            ParamMouthOpenY: 0.18 * wave,
            ParamMouthForm: -0.18 * wave,
            ParamHandLB: wave * 0.35,
            ParamHandRB: wave * 0.2
        };
    }
    if (name === "happy") {
        return {
            ParamBodyAngleX: doubleWave * 8.2,
            ParamBodyAngleY: 7.4 * wave,
            ParamBodyAngleZ: doubleWave * 4.2,
            ParamAngleZ: doubleWave * 5.2,
            ParamEyeLOpen: 1 - 0.42 * wave,
            ParamEyeROpen: 1 - 0.42 * wave,
            ParamEyeLSmile: wave,
            ParamEyeRSmile: wave,
            ParamCheek: 0.72 * wave,
            ParamMouthOpenY: 0.34 * wave,
            ParamMouthForm: 0.92 * wave,
            ParamHairAhoge: smallWave * 8,
            ParamShoulder: wave * 4,
            ParamArmLA: wave * 0.35,
            ParamArmRA: wave * 0.35
        };
    }
    if (name === "stretch") {
        return {
            ParamBodyAngleX: smallWave * 5.8,
            ParamBodyAngleY: 13.5 * wave,
            ParamBodyAngleZ: doubleWave * 4.4,
            ParamAngleY: -10 * wave,
            ParamEyeLOpen: 1 - 0.62 * wave,
            ParamEyeROpen: 1 - 0.62 * wave,
            ParamMouthOpenY: 0.28 * wave,
            ParamMouthForm: -0.18 * wave,
            ParamShoulder: 6.5 * wave,
            ParamLeg: 0.28 * wave,
            ParamArmLA: -0.55 * wave,
            ParamArmRA: -0.55 * wave
        };
    }
    if (name === "error") {
        return {
            ParamBodyAngleX: smallWave * 13,
            ParamBodyAngleZ: doubleWave * 8,
            ParamAngleX: smallWave * 12,
            ParamAngleZ: doubleWave * 9,
            ParamEyeLOpen: 1 - 0.28 * wave,
            ParamEyeROpen: 1 - 0.28 * wave,
            ParamBrowLAngle: -0.65 * wave,
            ParamBrowRAngle: 0.65 * wave,
            ParamMouthOpenY: 0.38 * wave,
            ParamMouthForm: -0.78 * wave,
            ParamHairAhoge: smallWave * 9,
            ParamShoulder: smallWave * 4.4
        };
    }
    if (name === "curious") {
        return {
            ParamBodyAngleX: 5.5 * wave,
            ParamAngleX: 10 * wave,
            ParamAngleY: 4.5 * wave,
            ParamEyeBallX: 0.6 * wave,
            ParamEyeBallY: 0.28 * wave,
            ParamBrowLY: 0.45 * wave,
            ParamBrowRY: -0.22 * wave,
            ParamMouthOpenY: 0.12 * wave,
            ParamMouthForm: 0.22 * wave
        };
    }
    if (name === "listening") {
        return {
            ParamBodyAngleX: -4.8 * wave,
            ParamBodyAngleY: -3.5 * wave,
            ParamAngleX: -8.5 * wave,
            ParamAngleY: -4.2 * wave,
            ParamEyeBallX: -0.45 * wave,
            ParamBrowLY: -0.28 * wave,
            ParamBrowRY: -0.28 * wave,
            ParamMouthOpenY: 0.08 * wave
        };
    }
    if (name === "speaking") {
        const envelope = Math.sin(progress * Math.PI);
        const mouthPulse = Math.pow(Math.abs(Math.sin(progress * Math.PI * 16)), 0.72);
        const mouthWave = (0.08 + mouthPulse * 0.56) * Math.max(0.18, envelope);
        const syllableWave = Math.sin(progress * Math.PI * 8);
        return {
            ParamBodyAngleX: doubleWave * 2.2,
            ParamBodyAngleY: 2.8 * wave,
            ParamBodyAngleZ: smallWave * 1.2,
            ParamAngleX: doubleWave * 3.2,
            ParamAngleY: 2.4 * wave,
            ParamEyeLOpen: 1 - 0.08 * wave,
            ParamEyeROpen: 1 - 0.08 * wave,
            ParamBrowLY: 0.12 * wave,
            ParamBrowRY: 0.12 * wave,
            ParamMouthOpenY: mouthWave,
            ParamMouthForm: 0.18 * syllableWave,
            ParamCheek: 0.12 * wave,
            ParamBreath: 0.45 + 0.34 * wave
        };
    }
    if (name === "shy") {
        return {
            ParamBodyAngleX: doubleWave * 4,
            ParamBodyAngleY: -7.5 * wave,
            ParamAngleY: -9 * wave,
            ParamEyeLOpen: 1 - 0.36 * wave,
            ParamEyeROpen: 1 - 0.36 * wave,
            ParamEyeBallY: -0.38 * wave,
            ParamCheek: wave,
            ParamMouthForm: 0.32 * wave,
            ParamBrowLY: -0.4 * wave,
            ParamBrowRY: -0.4 * wave
        };
    }
    if (name === "sleepy") {
        return {
            ParamBodyAngleX: doubleWave * 2.2,
            ParamBodyAngleY: -9.5 * wave,
            ParamAngleY: -10 * wave,
            ParamEyeLOpen: 1 - 0.86 * wave,
            ParamEyeROpen: 1 - 0.86 * wave,
            ParamMouthOpenY: 0.42 * wave,
            ParamMouthForm: -0.35 * wave,
            ParamBreath: 0.72 + 0.22 * wave,
            ParamShoulder: -2.4 * wave
        };
    }
    if (name === "surprise") {
        return {
            ParamBodyAngleY: 8.5 * wave,
            ParamAngleY: 9 * wave,
            ParamEyeLOpen: 1 + 0.35 * wave,
            ParamEyeROpen: 1 + 0.35 * wave,
            ParamBrowLY: 0.68 * wave,
            ParamBrowRY: 0.68 * wave,
            ParamMouthOpenY: 0.58 * wave,
            ParamMouthForm: -0.18 * wave,
            ParamHairAhoge: smallWave * 7
        };
    }
    if (name === "wave") {
        return {
            ParamBodyAngleX: doubleWave * 5.5,
            ParamBodyAngleY: 5 * wave,
            ParamAngleZ: doubleWave * 3.5,
            ParamEyeLSmile: 0.72 * wave,
            ParamEyeRSmile: 0.72 * wave,
            ParamMouthForm: 0.75 * wave,
            ParamMouthOpenY: 0.22 * wave,
            ParamArmRA: smallWave * 0.75,
            ParamArmRB: smallWave * 0.75,
            ParamHandRB: smallWave
        };
    }
    if (name === "proud") {
        return {
            ParamBodyAngleY: 8.8 * wave,
            ParamAngleY: 5.6 * wave,
            ParamEyeLOpen: 1 - 0.25 * wave,
            ParamEyeROpen: 1 - 0.25 * wave,
            ParamMouthForm: 0.88 * wave,
            ParamBrowLY: 0.24 * wave,
            ParamBrowRY: 0.24 * wave,
            ParamShoulder: 5 * wave
        };
    }
    const subtle = Boolean(options.subtle);
    return {
        ParamBodyAngleY: (subtle ? 4.2 : 6.2) * wave,
        ParamBodyAngleX: doubleWave * (subtle ? 2.8 : 4.2),
        ParamAngleZ: doubleWave * (subtle ? 2.2 : 3.4),
        ParamBreath: 0.4 + 0.4 * wave,
        ParamEyeLOpen: 1 - 0.16 * wave,
        ParamEyeROpen: 1 - 0.16 * wave,
        ParamMouthForm: 0.18 * wave,
        ParamHairAhoge: smallWave * 3.8
    };
}

function playBehavior(name = "idle", options = {}) {
    if (!model) return;
    stopBehaviorAnimation();
    stopReleasePhysics();
    const duration = typeof options.durationMs === "number"
        ? Math.max(450, Math.min(4000, options.durationMs))
        : name === "error" ? 1100
            : name === "happy" ? 1500
                : name === "stretch" || name === "sleepy" ? 2200
                    : name === "wave" ? 1700
                        : name === "curious" || name === "listening" || name === "shy" ? 1600
                            : 1500;
    activeBehavior = { name, options, duration };
    behaviorStartedAt = performance.now();
    tryExpression(expressionCandidatesForBehavior(name));
    const playedMotion = playBehaviorMotion(name);
    postMessageToHost({
        type: "behavior_debug",
        id: ++behaviorDebugSerial,
        name,
        duration,
        playedMotion,
        parameterCount: modelParameterIds.size,
        motionGroups: Array.from(modelMotionGroups.keys()),
        expressionCount: modelExpressionNames.size
    });
}

function modelLayoutMetrics() {
    const effectiveHeight = Math.min(window.innerHeight, MODEL_LAYOUT_BASE_HEIGHT);
    return {
        width: window.innerWidth,
        height: effectiveHeight,
        topInset: Math.max(0, window.innerHeight - effectiveHeight)
    };
}

function updateModelScale() {
    if (!modelNaturalSize) return;
    const metrics = modelLayoutMetrics();
    modelScale = Math.min(
        (metrics.height * MODEL_VERTICAL_SCALE_RATIO) / modelNaturalSize.height,
        (metrics.width * MODEL_HORIZONTAL_SCALE_RATIO) / modelNaturalSize.width
    );
}

function layoutModel() {
    if (!model || !modelNaturalSize) return;
    updateModelScale();
    if (modelScale === null) return;
    const metrics = modelLayoutMetrics();
    model.scale.set(modelScale);
    model.anchor.set(0.5, 0.5);
    model.position.set(metrics.width * 0.5, metrics.topInset + metrics.height * MODEL_VERTICAL_ANCHOR_RATIO);
    reportModelBounds();
}

function finishModelDrag(screenX, screenY) {
    const wasDragging = draggingModel;
    const endScreenX = typeof screenX === "number" ? screenX : lastDragScreenX;
    const endScreenY = typeof screenY === "number" ? screenY : lastDragScreenY;
    activePointerId = null;
    draggingModel = false;
    setModelPressing(false);
    setModelDragging(false);
    clearPendingDragMove();
    clearModelDragTimer();
    if (wasDragging) {
        postMessageToHost({
            type: "model_drag_end",
            screenX: endScreenX,
            screenY: endScreenY
        });
        startReleasePhysics();
    }
    lastDragScreenX = null;
    lastDragScreenY = null;
    lastDragMoveAt = 0;
}

function clearPendingDragMove() {
    pendingDragMove = null;
    if (dragMoveFrame !== null) {
        window.cancelAnimationFrame(dragMoveFrame);
        dragMoveFrame = null;
    }
}

function queueModelDragMove(screenX, screenY) {
    const now = performance.now();
    if (lastDragMoveAt > 0 && lastDragScreenX !== null && lastDragScreenY !== null) {
        const elapsed = Math.max(16, now - lastDragMoveAt);
        const vx = (screenX - lastDragScreenX) / elapsed;
        const vy = (screenY - lastDragScreenY) / elapsed;
        applyDragPhysics(vx * 180, vy * 120);
    }
    lastDragScreenX = screenX;
    lastDragScreenY = screenY;
    lastDragMoveAt = now;
    pendingDragMove = { screenX, screenY };
    if (dragMoveFrame !== null) return;
    dragMoveFrame = window.requestAnimationFrame(() => {
        dragMoveFrame = null;
        const point = pendingDragMove;
        pendingDragMove = null;
        if (!point || !draggingModel) return;
        postMessageToHost({
            type: "model_drag_move",
            screenX: point.screenX,
            screenY: point.screenY
        });
    });
}

function startPendingModelDrag(screenX = dragStartScreenX, screenY = dragStartScreenY) {
    if (!modelDragPending) return false;
    if (modelDragTimer !== null) {
        window.clearTimeout(modelDragTimer);
        modelDragTimer = null;
    }
    modelDragPending = false;
    draggingModel = true;
    setModelDragging(true);
    lastDragScreenX = screenX;
    lastDragScreenY = screenY;
    lastDragMoveAt = performance.now();
    postMessageToHost({
        type: "model_drag_start",
        screenX,
        screenY
    });
    return true;
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

async function loadModel(url = DEFAULT_MODEL_URL, options = {}) {
    const force = Boolean(options.force);
    const candidates = modelUrlCandidates(url);
    const loadingKey = candidates.join("|");
    if (!force && model && loadedModelUrl && candidates.includes(loadedModelUrl)) {
        postMessageToHost({ type: "loaded", url: loadedModelUrl });
        return;
    }
    if (!force && loadingModelKey === loadingKey) return;
    const currentToken = ++loadToken;
    loadingModelKey = loadingKey;
    try {
        if (!PIXI?.live2d?.Live2DModel) {
            throw new Error("Live2D runtime is not ready");
        }
        if (model) {
            clearIdleBehaviorTimer();
            stopBehaviorAnimation();
            stopReleasePhysics();
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
                app.stage.addChild(model);
                layoutModel();

                const ctrl = model.internalModel.focusController;
                if (ctrl) {
                    ctrl.acceleration = 0.04;
                    ctrl.deceleration = 0.08;
                }

                refreshModelCapabilities();
                installModelOverlay(model);
                ambientStartedAt = performance.now();
                layoutModel();
                model.interactive = true;
                reportModelBounds();
                window.setTimeout(() => playBehavior("wave", { durationMs: 2300 }), 120);
                window.setTimeout(() => playBehavior("curious", { durationMs: 1600 }), 2600);
                scheduleIdleBehavior();

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
        postMessageToHost({ type: "error", message: errorMessage(error) });
        console.error(error);
    }
}

listenHostMessages((msg) => {
    if (!msg || typeof msg !== "object") return;
    if (msg.source !== HOST_MESSAGE_SOURCE) return;
    switch (msg.type) {
        case "load":
            void loadModel(msg.url, { force: Boolean(msg.force) });
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
        case "behavior":
            playBehavior(msg.name, msg.options);
            break;
        case "pose":
            applyParameterPose(msg.params, typeof msg.weight === "number" ? msg.weight : 0.5);
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
    }
});

canvas.addEventListener("contextmenu", (event) => {
    if (!pointInModelBounds(event.clientX, event.clientY)) return;
    event.preventDefault();
    clearModelDragTimer();
    finishModelDrag();
});

canvas.addEventListener("dblclick", (event) => {
    clearModelDragTimer();
    if (!pointInModelBounds(event.clientX, event.clientY)) return;
    clearTapTimer();
    postMessageToHost({ type: "toggle_main_window", areas: ["model"] });
});

canvas.addEventListener("pointermove", (event) => {
    const overModel = pointInModelBounds(event.clientX, event.clientY);
    setModelHover(overModel);
    if (!draggingModel) {
        focusScreenPoint(event.clientX, event.clientY, false);
    }
    if (modelDragPending && activePointerId === event.pointerId) {
        dragStartScreenX = event.screenX;
        dragStartScreenY = event.screenY;
        if (Math.hypot(event.screenX - dragOriginScreenX, event.screenY - dragOriginScreenY) >= MODEL_DRAG_START_MOVE_PX) {
            startPendingModelDrag(event.screenX, event.screenY);
            queueModelDragMove(event.screenX, event.screenY);
        }
        return;
    }
    if (draggingModel && activePointerId === event.pointerId) {
        queueModelDragMove(event.screenX, event.screenY);
    }
});

canvas.addEventListener("pointerleave", () => {
    if (draggingModel) return;
    setModelHover(false);
    if (!modelDragPending) setModelPressing(false);
});

canvas.addEventListener("pointerdown", (event) => {
    if (event.button !== 0 || !pointInModelBounds(event.clientX, event.clientY)) return;
    event.preventDefault();
    clearTapTimer();
    stopBehaviorAnimation();
    stopReleasePhysics();
    activePointerId = event.pointerId;
    dragOriginScreenX = event.screenX;
    dragOriginScreenY = event.screenY;
    dragStartScreenX = event.screenX;
    dragStartScreenY = event.screenY;
    lastDragScreenX = event.screenX;
    lastDragScreenY = event.screenY;
    lastDragMoveAt = performance.now();
    canvas.setPointerCapture?.(event.pointerId);
    setModelHover(true);
    setModelPressing(true);
    clearModelDragTimer();
    modelDragPending = true;
    modelDragTimer = window.setTimeout(() => {
        if (!modelDragPending || activePointerId !== event.pointerId) return;
        startPendingModelDrag(dragStartScreenX, dragStartScreenY);
    }, MODEL_DRAG_DELAY_MS);
});

canvas.addEventListener("pointerup", (event) => {
    if (activePointerId !== null && activePointerId !== event.pointerId) return;
    canvas.releasePointerCapture?.(event.pointerId);
    const clientX = event.clientX;
    const clientY = event.clientY;
    const wasPendingTap = modelDragPending && !draggingModel && pointInModelBounds(event.clientX, event.clientY);
    finishModelDrag(event.screenX, event.screenY);
    if (wasPendingTap) {
        scheduleModelTap(clientX, clientY);
    }
});
canvas.addEventListener("pointercancel", (event) => {
    clearTapTimer();
    finishModelDrag(event.screenX, event.screenY);
});
canvas.addEventListener("lostpointercapture", (event) => {
    finishModelDrag(event.screenX, event.screenY);
});
window.addEventListener("blur", () => {
    clearTapTimer();
    finishModelDrag();
});
window.addEventListener("resize", layoutModel);

postMessageToHost({ type: "ready" });
