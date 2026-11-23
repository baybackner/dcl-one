import type { DeepReadonlyObject, IEngine, PBVideoPlayer, Entity } from '@dcl/ecs';
import ReactEcs, { UiEntity } from '@dcl/react-ecs';
import { openExternalUrl } from '~system/RestrictedActions';
import { COLORS, RADIUS, SPACING, TYPE } from '../theme';
import { SectionHeader, FieldLabel, Icon, TextField } from '../Primitives';
import { PillButton } from '../Controls';
import { VolumeSlider } from './VolumeSlider';
import { docsUrl } from '../fetch-utils';
import { createVideoPlayerControls, isVideoUrl, urlAction } from './utils';

const VIDEO_PLAYER_HELP = '/creator/scene-editor/interactivity/video-screen';

export function VideoControlURL({
  engine,
  video,
  entity,
}: {
  engine: IEngine;
  entity: Entity;
  video: DeepReadonlyObject<PBVideoPlayer> | undefined;
}) {
  const [videoURL, setVideoURL] = ReactEcs.useState('');
  const url = video?.src?.startsWith('livekit-video://') ? '' : (video?.src ?? '');
  ReactEcs.useEffect(() => setVideoURL(url), [entity]);
  // The URL that any admin puts on the screen is the field's text on every panel.
  ReactEcs.useEffect(() => {
    if (url) setVideoURL(url);
  }, [url]);
  const controls = createVideoPlayerControls(entity, engine);
  const isActive = !!(video && isVideoUrl(video.src));
  const primaryLabel = urlAction(video?.src, videoURL);

  return (
    <UiEntity uiTransform={{ flexDirection: 'column', width: '100%' }}>
      <SectionHeader
        title="Video URL"
        right={
          <UiEntity
            uiTransform={{ width: 16, height: 16 }}
            uiBackground={{ color: COLORS.transparent }}
            onMouseDown={() => openExternalUrl({ url: docsUrl(VIDEO_PLAYER_HELP) })}
          >
            <Icon
              name="help"
              size={16}
              color={COLORS.textSecondary}
            />
          </UiEntity>
        }
      />
      <UiEntity
        uiTransform={{ width: '100%', height: 20, margin: { top: SPACING.lg, bottom: SPACING.xl } }}
        uiText={{
          value: 'Paste an .m3u8 video URL to play it on this screen.',
          fontSize: TYPE.label,
          color: COLORS.textSecondary,
          textAlign: 'top-left',
          textWrap: 'wrap',
        }}
      />

      <UiEntity uiTransform={{ flexDirection: 'row', alignItems: 'center', width: '100%' }}>
        <TextField
          onChange={setVideoURL}
          value={videoURL}
          placeholder="Paste your video URL"
          uiTransform={{ flexGrow: 1, flexBasis: 0, minWidth: 0, margin: { right: SPACING.md } }}
        />
        <PillButton
          id="video_control_url_activate"
          label={primaryLabel}
          variant="filled"
          disabled={primaryLabel !== 'Deactivate' && !isVideoUrl(videoURL)}
          uiTransform={{ flexShrink: 0 }}
          onClick={() => controls.setSource(primaryLabel === 'Deactivate' ? '' : videoURL)}
        />
      </UiEntity>

      <UiEntity
        uiTransform={{ flexDirection: 'column', width: '100%', margin: { top: SPACING.xl } }}
      >
        <FieldLabel text="Playback" />
        <UiEntity uiTransform={{ flexDirection: 'row', alignItems: 'center', width: '100%' }}>
          <PillButton
            id="video_control_play"
            label="Play"
            iconName="play"
            variant="outlined"
            disabled={!isActive}
            uiTransform={{
              flexGrow: 1,
              flexBasis: 0,
              height: 34,
              padding: { left: 4, right: 4 },
              margin: { right: SPACING.md },
            }}
            onClick={() => controls.play()}
          />
          <PillButton
            id="video_control_pause"
            label="Pause"
            iconName="pause"
            variant="outlined"
            disabled={!isActive}
            uiTransform={{
              flexGrow: 1,
              flexBasis: 0,
              height: 34,
              padding: { left: 4, right: 4 },
              margin: { right: SPACING.md },
            }}
            onClick={() => controls.pause()}
          />
          <PillButton
            id="video_control_restart"
            label="Restart"
            iconName="refresh"
            variant="outlined"
            disabled={!isActive}
            uiTransform={{
              flexGrow: 1,
              flexBasis: 0,
              height: 34,
              padding: { left: 4, right: 4 },
              margin: { right: SPACING.md },
            }}
            onClick={() => controls.restart()}
          />
          <UiEntity
            uiTransform={{
              width: 36,
              height: 34,
              borderRadius: RADIUS.md,
              borderWidth: 1,
              borderColor: COLORS.outline,
              alignItems: 'center',
              justifyContent: 'center',
            }}
            uiBackground={{ color: video?.loop ? COLORS.primary : COLORS.transparent }}
            onMouseDown={() => isActive && controls.setLoop(!video?.loop)}
          >
            <Icon
              name="loop"
              size={15}
              color={
                !isActive ? COLORS.textDisabled : video?.loop ? COLORS.white : COLORS.textTertiary
              }
            />
          </UiEntity>
        </UiEntity>
      </UiEntity>

      <VolumeSlider
        engine={engine}
        entity={entity}
        video={video}
      />
    </UiEntity>
  );
}
