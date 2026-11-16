// Step clock shared by the horizontal and vertical models. Both integrate in fixed steps
// (FIXED_STEP); this module turns the engine's tick timing into the time each tick covers
// and the reply delay to predict over.
import { DELAY_ROUNDING, FRAME_TIME_SMOOTHING, MAX_TICK_TIME, PREDICT_MODE } from './constants';

// Time to add to the step accumulators this tick: from the end of the previous tick's frame
// to the end of this tick's frame.
export var tickAdvance = 0;
// Duration of this tick's frame (AvatarMovementInfo.stepTime).
export var frameTime = 0;
// Frames the reply is expected to wait before the engine applies it (0..2).
export var replyDelay = 0;

var lastFrameTime = 0;
var meanFrameTime = 0;
var meanRatio = 1;

// tickTime: engine time since the previous tick (previousStepTime, clamped), which ends at
// the frame before this one. stepTime: this frame's duration.
export function beginTick(tickTime: number, stepTime: number, previousStepTime: number) {
  frameTime = Math.max(0, stepTime);
  tickAdvance = Math.max(0, tickTime + frameTime - lastFrameTime);
  lastFrameTime = frameTime;

  // previousStepTime spans the previous dispatch frame through the frame its reply landed on,
  // so its ratio to the frame time is 1 + that reply's delay. Frame times jitter, so the frame
  // time is smoothed (mode 1), or the ratio itself is (mode 2).
  replyDelay = 0;
  if (PREDICT_MODE === 0 || frameTime <= 0 || previousStepTime <= 0) return;
  meanFrameTime = meanFrameTime > 0 ? meanFrameTime + (frameTime - meanFrameTime) * FRAME_TIME_SMOOTHING : frameTime;
  const ratio = Math.min(previousStepTime, MAX_TICK_TIME) / meanFrameTime;
  meanRatio += (ratio - meanRatio) * FRAME_TIME_SMOOTHING;
  const r = PREDICT_MODE === 2 ? meanRatio : ratio;
  replyDelay = Math.max(0, Math.min(2, Math.round(r - DELAY_ROUNDING) - 1));
}
