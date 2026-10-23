// Runtime-only bindings for CoreGraphics' private virtual-display classes.
// ABI reference: Chromium ui/display/mac/test/virtual_display_util_mac.mm and
// DeskPad/CGVirtualDisplayPrivate.h. Missing classes/selectors fail closed.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <objc/message.h>

@interface NSObject (DLVirtualInitializers)
- (void)setDispatchQueue:(dispatch_queue_t)queue;
- (instancetype)initWithDescriptor:(id)descriptor;
- (instancetype)initWithWidth:(NSUInteger)width height:(NSUInteger)height refreshRate:(double)rate;
@end

static id display;
static uint32_t displayID;
static NSString *lastError;
static BOOL configured;
static uint32_t pixelWidth, pixelHeight, displayScale;

static id make(NSString *name) {
  Class cls = NSClassFromString(name);
  return cls ? [[cls alloc] init] : nil;
}
static BOOL supports(id object, NSString *selector) {
  return object && [object respondsToSelector:NSSelectorFromString(selector)];
}
const char *dl_virtual_error(void) { return lastError.UTF8String ?: "Virtual display unavailable"; }
uint32_t dl_virtual_id(void) {
  CGDirectDisplayID ids[64]; uint32_t count = 0;
  if (!configured || !display || CGGetOnlineDisplayList(64, ids, &count) != kCGErrorSuccess) return 0;
  for (uint32_t i = 0; i < count; i++) if (ids[i] == displayID) {
    CGDisplayModeRef mode = CGDisplayCopyDisplayMode(displayID);
    BOOL matches = mode && CGDisplayModeGetPixelWidth(mode) == pixelWidth
        && CGDisplayModeGetPixelHeight(mode) == pixelHeight
        && CGDisplayModeGetWidth(mode) == pixelWidth / displayScale
        && CGDisplayModeGetHeight(mode) == pixelHeight / displayScale;
    if (mode) CFRelease(mode);
    return matches ? displayID : 0;
  }
  return 0;
}
bool dl_virtual_supported(void) {
  return NSClassFromString(@"CGVirtualDisplay") && NSClassFromString(@"CGVirtualDisplayDescriptor")
      && NSClassFromString(@"CGVirtualDisplaySettings") && NSClassFromString(@"CGVirtualDisplayMode");
}
uint32_t dl_virtual_configure(uint32_t width, uint32_t height, uint32_t scale, uint32_t serial) {
  @try {
    lastError = nil;
    if (!dl_virtual_supported() || width < 256 || height < 256 || width > 4096 || height > 4096
        || (uint64_t)width * height > 8000000 || (scale != 1 && scale != 2)
        || width % scale || height % scale) {
      lastError = @"Virtual display API or mode unavailable"; return 0;
    }
    configured = NO;
    BOOL created = !display;
    if (created) {
      id descriptor = make(@"CGVirtualDisplayDescriptor");
      if (!supports(descriptor, @"setDispatchQueue:") || !supports(descriptor, @"setSizeInMillimeters:")) {
        lastError = @"Virtual display descriptor unavailable"; return 0;
      }
      [descriptor setValue:@"Daat Locus" forKey:@"name"];
      [descriptor setValue:@4096 forKey:@"maxPixelsWide"];
      [descriptor setValue:@4096 forKey:@"maxPixelsHigh"];
      [descriptor setValue:[NSValue valueWithSize:NSMakeSize(400, 250)] forKey:@"sizeInMillimeters"];
      [descriptor setValue:@0x3456 forKey:@"vendorID"];
      [descriptor setValue:@0x1234 forKey:@"productID"];
      [descriptor setValue:@(serial ?: 1) forKey:@"serialNum"];
      [descriptor setDispatchQueue:dispatch_get_main_queue()];
      id allocated = [NSClassFromString(@"CGVirtualDisplay") alloc];
      if (!supports(allocated, @"initWithDescriptor:")) {lastError = @"Virtual display initializer unavailable"; return 0;}
      display = [allocated initWithDescriptor:descriptor];
      if (!display) {lastError = @"Cannot create virtual display"; return 0;}
      displayID = [[display valueForKey:@"displayID"] unsignedIntValue];
    }
    id mode = [NSClassFromString(@"CGVirtualDisplayMode") alloc];
    if (!supports(mode, @"initWithWidth:height:refreshRate:") || !supports(display, @"applySettings:")) {
      lastError = @"Virtual display mode API unavailable"; return 0;
    }
    mode = [mode initWithWidth:width / scale height:height / scale refreshRate:60.0];
    id settings = make(@"CGVirtualDisplaySettings");
    [settings setValue:@(scale == 2) forKey:@"hiDPI"];
    [settings setValue:@[mode] forKey:@"modes"];
    if (!((BOOL(*)(id,SEL,id))objc_msgSend)(display, NSSelectorFromString(@"applySettings:"), settings)) {
      lastError = @"Cannot apply virtual display mode"; return 0;
    }
    // WindowServer publishes new modes asynchronously. Do not read its stale
    // display cache immediately after applySettings, or report the old mode.
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:6];
    BOOL selected = NO;
    BOOL requested = NO;
    while ([deadline timeIntervalSinceNow] > 0) {
      CGDisplayModeRef current = CGDisplayCopyDisplayMode(displayID);
      selected = current && CGDisplayModeGetPixelWidth(current) == width
          && CGDisplayModeGetPixelHeight(current) == height
          && CGDisplayModeGetWidth(current) == width / scale
          && CGDisplayModeGetHeight(current) == height / scale;
      // Applying a HiDPI flag can leave the previous logical mode selected
      // when both modes share the same pixel dimensions. Select the published
      // mode explicitly, on this owned output only, at most once per request.
      if (current && !selected && !requested) {
        CFArrayRef modes = CGDisplayCopyAllDisplayModes(displayID,
            (__bridge CFDictionaryRef)@{(__bridge NSString *)kCGDisplayShowDuplicateLowResolutionModes: @YES});
        if (modes) {
          for (CFIndex i = 0; i < CFArrayGetCount(modes); i++) {
            CGDisplayModeRef candidate = (CGDisplayModeRef)CFArrayGetValueAtIndex(modes, i);
            if (CGDisplayModeGetPixelWidth(candidate) == width
                && CGDisplayModeGetPixelHeight(candidate) == height
                && CGDisplayModeGetWidth(candidate) == width / scale
                && CGDisplayModeGetHeight(candidate) == height / scale) {
              requested = YES;
              CGError error = CGDisplaySetDisplayMode(displayID, candidate, NULL);
              if (error != kCGErrorSuccess) {
                CFRelease(modes); CFRelease(current);
                lastError = @"Cannot select virtual display mode"; return 0;
              }
              break;
            }
          }
          CFRelease(modes);
        }
      }
      if (current) CFRelease(current);
      if (selected) break;
      if ([NSThread isMainThread]) [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.05]];
      else [NSThread sleepForTimeInterval:.05];
    }
    if (!selected) {lastError = @"Virtual display mode did not settle"; return 0;}
    if (created) {
      CGDirectDisplayID ids[64]; uint32_t count = 0; CGFloat right = 0;
      if (CGGetOnlineDisplayList(64, ids, &count) != kCGErrorSuccess) {lastError = @"Cannot inspect display arrangement"; return 0;}
      for (uint32_t i = 0; i < count; i++) if (ids[i] != displayID) right = MAX(right, CGRectGetMaxX(CGDisplayBounds(ids[i])));
      CGDisplayConfigRef config;
      if (CGBeginDisplayConfiguration(&config) != kCGErrorSuccess) {lastError = @"Cannot prepare display arrangement"; return 0;}
      CGError err = CGConfigureDisplayOrigin(config, displayID, (int32_t)right + 64, 0);
      if (err != kCGErrorSuccess) CGCancelDisplayConfiguration(config);
      else err = CGCompleteDisplayConfiguration(config, kCGConfigureForSession);
      if (err != kCGErrorSuccess) {lastError = @"Cannot arrange virtual display"; return 0;}
    }
    pixelWidth = width; pixelHeight = height; displayScale = scale; configured = YES;
    return displayID;
  } @catch (NSException *exception) {
    lastError = @"Virtual display API is incompatible with this macOS version";
    return 0;
  }
}
void dl_virtual_stop(void) { configured = NO; display = nil; displayID = 0; }
