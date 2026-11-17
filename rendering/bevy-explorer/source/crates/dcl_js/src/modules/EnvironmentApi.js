module.exports.getBootstrapData = async function (body) {
    const sceneInfo = await Deno.core.ops.op_scene_information()
    return {
        id: sceneInfo.urn,
        baseUrl: sceneInfo.baseUrl,
        entity: {
            content: sceneInfo.content,
            metadataJson: sceneInfo.metadataJson
        },
        useFPSThrottling: false,
    }
}

module.exports.isPreviewMode = async function (body) {
    const realm = await Deno.core.ops.op_realm_information()
    return {
        isPreview: realm.isPreview
    }
}

module.exports.getPlatform = async function (body) {
    return {
        platform: Deno.core.ops.op_get_platform()
    }
}
module.exports.areUnsafeRequestAllowed = async function (body) {
    return {
        status: false
    }
}
module.exports.getCurrentRealm = async function (body) {
    const realm = await Deno.core.ops.op_realm_information()
    return {
        currentRealm: {
            protocol: 'v3',
            layer: '', // layer doesn't exists anymore
            room: '',
            serverName: realm.realmName,
            displayName: realm.realmName,
            domain: realm.baseUrl,
        }
    }
}
// `avatarMovementAssist`: what the engine's collide-and-slide does for the avatar on the ground,
// comma-separated, so the movement controller scene leaves it to the engine (`slopeLimit`: a
// surface too steep to stand on stops the move into it; `snap`: the move follows walkable ground,
// up a slope at its horizontal speed and down a slope or a low step; `step`: a riser up to 0.45m
// is stepped onto)
module.exports.getExplorerConfiguration = async function (body) {
    return {
        clientUri: 'bevy-explorer',
        configurations: { avatarMovementAssist: 'slopeLimit,snap,step' }
    }
}
module.exports.getDecentralandTime = async function (body) {
    const res = await Deno.core.ops.op_world_time();
    return res;
}
