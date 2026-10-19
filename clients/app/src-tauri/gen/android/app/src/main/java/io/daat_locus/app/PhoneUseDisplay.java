package io.daat_locus.app;

import org.json.JSONObject;

/** Owner preference for display creation and resizing, never a model-supplied size. */
final class PhoneUseDisplay {
    static final PhoneUseDisplay DEFAULT = new PhoneUseDisplay(1280, 720, 160);
    static final long MAX_PIXELS = 8_000_000;
    final int width, height, density;
    PhoneUseDisplay(int width, int height, int density) {
        if (width < 256 || width > 4096 || height < 256 || height > 4096
                || (long) width * height > MAX_PIXELS || density < 80 || density > 640)
            throw new IllegalArgumentException("Invalid virtual display dimensions or density");
        this.width = width; this.height = height; this.density = density;
    }
    static PhoneUseDisplay fromJson(JSONObject value) throws Exception {
        return new PhoneUseDisplay(value.getInt("width"), value.getInt("height"), value.getInt("density"));
    }
    JSONObject json() {
        try { return new JSONObject().put("width", width).put("height", height).put("density", density); }
        catch (org.json.JSONException impossible) { throw new IllegalStateException(impossible); }
    }
}
