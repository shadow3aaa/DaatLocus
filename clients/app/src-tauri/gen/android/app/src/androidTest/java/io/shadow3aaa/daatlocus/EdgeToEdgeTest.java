package io.shadow3aaa.daatlocus;

import android.app.Activity;
import android.content.pm.ActivityInfo;
import android.graphics.Bitmap;
import android.os.SystemClock;
import android.os.Build;
import android.view.View;
import android.view.MotionEvent;
import android.view.InputDevice;
import android.view.inputmethod.InputMethodManager;
import android.webkit.WebView;
import android.view.WindowInsets;
import java.io.File;
import java.io.FileOutputStream;
import org.json.JSONObject;

/** Opt-in layout transitions against the actual Android WebView and system IME. */
final class EdgeToEdgeTest {
    private final ReleaseStartupTest test;
    private final Activity activity;
    private EdgeToEdgeTest(ReleaseStartupTest test, Activity activity) { this.test=test; this.activity=activity; }
    static void run(ReleaseStartupTest test, Activity activity) throws Exception { new EdgeToEdgeTest(test,activity).run(); }
    static void checkWindow(ReleaseStartupTest test, Activity activity) throws Exception {
        final float[] expected=new float[4];
        final String[] failure=new String[1];
        test.runOnMainSync(()->{
            View decor=activity.getWindow().getDecorView();
            View root=activity.findViewById(android.R.id.content);
            WebView web=ReleaseStartupTest.findWebView(root);
            int[] outer=new int[2], inner=new int[2];
            decor.getLocationOnScreen(outer); web.getLocationOnScreen(inner);
            if (inner[0]!=outer[0] || inner[1]!=outer[1] || web.getWidth()!=decor.getWidth() ||
                web.getHeight()!=decor.getHeight() || root.getPaddingTop()!=0 || root.getPaddingBottom()!=0) {
                failure[0]="WebView does not fill the system-bar areas: "+inner[0]+","+inner[1]+" "+web.getWidth()+"x"+web.getHeight()+"; window="+decor.getWidth()+"x"+decor.getHeight();
            }
            if (Build.VERSION.SDK_INT>=29 && (activity.getWindow().isNavigationBarContrastEnforced() ||
                activity.getWindow().isStatusBarContrastEnforced())) {
                failure[0]="System contrast scrims obscure the WebView background";
            }
            WindowInsets insets=root.getRootWindowInsets();
            float density=activity.getResources().getDisplayMetrics().density;
            if (Build.VERSION.SDK_INT>=30) {
                android.graphics.Insets bars=insets.getInsets(WindowInsets.Type.systemBars()|WindowInsets.Type.displayCutout());
                expected[0]=bars.top/density; expected[1]=bars.right/density;
                expected[2]=bars.bottom/density; expected[3]=bars.left/density;
            } else {
                expected[0]=insets.getSystemWindowInsetTop()/density; expected[1]=insets.getSystemWindowInsetRight()/density;
                expected[2]=insets.getSystemWindowInsetBottom()/density; expected[3]=insets.getSystemWindowInsetLeft()/density;
                if (Build.VERSION.SDK_INT>=28 && insets.getDisplayCutout()!=null) {
                    android.view.DisplayCutout cutout=insets.getDisplayCutout();
                    expected[0]=Math.max(expected[0],cutout.getSafeInsetTop()/density);
                    expected[1]=Math.max(expected[1],cutout.getSafeInsetRight()/density);
                    expected[2]=Math.max(expected[2],cutout.getSafeInsetBottom()/density);
                    expected[3]=Math.max(expected[3],cutout.getSafeInsetLeft()/density);
                }
            }
        });
        if (failure[0]!=null) throw new AssertionError(failure[0]);
        String values=java.util.Arrays.toString(expected);
        String matches="(()=>{let s=getComputedStyle(document.documentElement);return ['top','right','bottom','left'].every((k,i)=>Math.abs(parseFloat(s.getPropertyValue('--dl-safe-'+k))-"+values+"[i])<1);})()";
        long deadline=SystemClock.elapsedRealtime()+5000;
        while (!Boolean.TRUE.equals(test.evaluate(activity,matches))) {
            if (SystemClock.elapsedRealtime()>=deadline) throw new AssertionError("Web safe areas differ from actual Android system bars");
            SystemClock.sleep(50);
        }
    }
    private Object js(String script) throws Exception { return test.evaluate(activity,script); }
    private void await(String expression) throws Exception {
        long deadline=SystemClock.elapsedRealtime()+15000;
        while (SystemClock.elapsedRealtime()<deadline) {
            if (Boolean.TRUE.equals(js(expression))) return;
            SystemClock.sleep(100);
        }
        final String[] nativeState=new String[1];
        test.runOnMainSync(()->{
            View root=activity.findViewById(android.R.id.content);
            WebView web=ReleaseStartupTest.findWebView(root);
            WindowInsets insets=root.getRootWindowInsets();
            nativeState[0]="root="+root.getHeight()+", bottomPadding="+root.getPaddingBottom()+", web="+web.getHeight()+", ime="+insets.getInsets(WindowInsets.Type.ime())+", visible="+insets.isVisible(WindowInsets.Type.ime());
        });
        throw new AssertionError("Android layout transition timed out: "+expression+"; "+nativeState[0]+"; web="+js("JSON.stringify({height:innerHeight,visual:visualViewport.height,app:document.querySelector('.dl-app').getBoundingClientRect().height})"));
    }
    private void route(String path) throws Exception {
        js("history.pushState(null,'',"+JSONObject.quote(path)+");window.dispatchEvent(new PopStateEvent('popstate'));null");
    }
    private void tap(String selector) throws Exception {
        JSONObject point=new JSONObject((String)js("(()=>{let r=document.querySelector("+JSONObject.quote(selector)+").getBoundingClientRect();return JSON.stringify({x:r.x+r.width/2,y:r.y+r.height/2});})()"));
        test.runOnMainSync(()->{
            WebView web=ReleaseStartupTest.findWebView(activity.getWindow().getDecorView());
            web.requestFocus();
            float density=activity.getResources().getDisplayMetrics().density;
            long now=SystemClock.uptimeMillis();
            for(int action:new int[]{MotionEvent.ACTION_DOWN,MotionEvent.ACTION_UP}) {
                MotionEvent event=MotionEvent.obtain(now,now+action*40,action,(float)point.optDouble("x")*density,(float)point.optDouble("y")*density,0);
                event.setSource(InputDevice.SOURCE_TOUCHSCREEN); web.dispatchTouchEvent(event); event.recycle();
            }
        });
    }
    private void safeControl(String selector) throws Exception {
        await("(()=>{let e=document.querySelector("+JSONObject.quote(selector)+");if(!e)return false;let r=e.getBoundingClientRect(),s=getComputedStyle(document.documentElement),n=k=>parseFloat(s.getPropertyValue('--dl-safe-'+k))||0;return r.top>=n('top')-1&&r.left>=n('left')-1&&r.right<=innerWidth-n('right')+1&&r.bottom<=innerHeight-n('bottom')+1;})()");
    }
    private void nativeGeometry() throws Exception {
        checkWindow(test,activity);
        await("Math.abs(document.querySelector('.dl-app').getBoundingClientRect().height-innerHeight)<2");
    }
    private void keyboardGeometry() throws Exception {
        final int[] pixels=new int[3];
        test.runOnMainSync(()->{
            View root=activity.findViewById(android.R.id.content);
            WindowInsets insets=root.getRootWindowInsets();
            pixels[0]=root.getHeight(); pixels[1]=insets.getInsets(WindowInsets.Type.ime()).bottom;
            pixels[2]=insets.isVisible(WindowInsets.Type.ime())?1:0;
        });
        PhoneUseTest.require(pixels[2]==1 && pixels[1]>0,"System IME was not shown");
        // Floating/physical-keyboard modes can report only a short bottom strip.
        // Compare with actual OS geometry, never an assumed keyboard height.
        double expected=(pixels[0]-pixels[1])/activity.getResources().getDisplayMetrics().density;
        await("Math.abs(innerHeight-"+expected+")<2 && Math.abs(document.querySelector('.dl-app').getBoundingClientRect().height-innerHeight)<2");
    }
    private void screenshot(String name) throws Exception {
        await("document.getAnimations().filter(a=>a.effect?.getComputedTiming().iterations!==Infinity).every(a=>a.playState!=='running')");
        java.util.concurrent.CountDownLatch done=new java.util.concurrent.CountDownLatch(1);
        final int[] result=new int[]{-1};
        test.runOnMainSync(()->{
            View view=activity.getWindow().getDecorView();
            Bitmap bitmap=Bitmap.createBitmap(view.getWidth(),view.getHeight(),Bitmap.Config.ARGB_8888);
            WebView web=ReleaseStartupTest.findWebView(view);
            web.postVisualStateCallback(0,new WebView.VisualStateCallback(){ @Override public void onComplete(long id) {
                view.getViewTreeObserver().registerFrameCommitCallback(()->android.view.PixelCopy.request(activity.getWindow(),bitmap,status->{
                    result[0]=status;
                    try(FileOutputStream out=new FileOutputStream(new File(activity.getCacheDir(),"edge-"+name+".png"))) {
                        bitmap.compress(Bitmap.CompressFormat.PNG,100,out);
                    } catch(Exception error) { result[0]=-2; }
                    finally { bitmap.recycle(); done.countDown(); }
                },new android.os.Handler(android.os.Looper.getMainLooper())));
                view.invalidate();
            }});
        });
        PhoneUseTest.require(done.await(5,java.util.concurrent.TimeUnit.SECONDS)&&result[0]==android.view.PixelCopy.SUCCESS,"Native compositor screenshot failed");
    }
    private void run() throws Exception {
        int orientation=activity.getRequestedOrientation();
        try {
            test.runOnMainSync(()->activity.setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_PORTRAIT));
            await("innerHeight>innerWidth");
            route("/settings/gui"); await("document.querySelector('.dl-settings-page-heading')?.getBoundingClientRect().width>0");
            nativeGeometry(); safeControl(".dl-settings-page-heading button"); screenshot("settings");
            // The settings list, search field and keyboard have their own flow.
            js("document.querySelector('.dl-settings-page-heading button').click();null");
            await("document.querySelector('.dl-settings-page').dataset.mobileDetail!=='true'");
            safeControl(".dl-settings-sidebar header button");
            double height=((Number)js("innerHeight")).doubleValue();
            tap(".dl-settings-search input");
            await("document.activeElement.matches('.dl-settings-search input')");
            test.runOnMainSync(()->{
                WebView web=ReleaseStartupTest.findWebView(activity.getWindow().getDecorView());
                ((InputMethodManager)activity.getSystemService(Activity.INPUT_METHOD_SERVICE)).showSoftInput(web,InputMethodManager.SHOW_FORCED);
            });
            await("innerHeight<"+(height-1)); keyboardGeometry(); safeControl(".dl-settings-search input"); screenshot("keyboard");
            js("document.activeElement.blur();null");
            test.runOnMainSync(()->((InputMethodManager)activity.getSystemService(Activity.INPUT_METHOD_SERVICE)).hideSoftInputFromWindow(activity.getWindow().getDecorView().getWindowToken(),0));
            await("Math.abs(innerHeight-"+height+")<2");
            route("/"); await("!!document.querySelector('.dl-open-sidebar')");
            safeControl(".dl-open-sidebar");
            js("document.querySelector('.dl-open-sidebar').click();null"); await("!!document.querySelector('.dl-sidebar-drawer')");
            safeControl(".dl-sidebar-heading button"); screenshot("drawer");
            js("document.querySelector('.dl-sidebar-heading button').click();null");
            await("!document.querySelector('.dl-sidebar-drawer')");
            test.runOnMainSync(()->activity.setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE));
            await("innerWidth>innerHeight"); route("/settings/gui");
            await("!!document.querySelector('#dl-phone-use-enabled:not(:disabled)')"); nativeGeometry();
            safeControl(".dl-settings-page-heading h1");
            // Open and dismiss only: authorization remains an explicit Owner choice.
            js("document.querySelector('#dl-phone-use-enabled').click();null");
            await("!!document.querySelector('#dl-phone-use-method')");
            safeControl("[data-slot=dialog-content] [data-slot=dialog-close]"); screenshot("landscape-dialog");
            js("document.querySelector('[data-slot=dialog-content] [data-slot=dialog-close]').click();null");
            await("!document.querySelector('#dl-phone-use-method')");
        } finally {
            test.runOnMainSync(()->activity.setRequestedOrientation(orientation));
            route("/");
        }
    }
}
