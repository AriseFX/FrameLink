#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef UNICODE
#define UNICODE
#endif
#ifndef _UNICODE
#define _UNICODE
#endif
#include "capture.hpp"
#include <windowsx.h>
#include <commctrl.h>
#include <shellapi.h>
#include <shlobj.h>
#include <fstream>
#include <map>
#include <vector>
#include <sstream>

using namespace cast;
constexpr UINT TrayMessage = WM_APP + 1;
enum { NameId=100, XId, YId, WidthId, HeightId, FpsId, UnlimitedId, BackendId, CursorId, PreviewId, MonitorId, FullId, PickId, StartId, StopId, TrayId, LinkId, AboutId, FormatId, CenterId, PrecisionId=120, ExitId=121 };
HICON appIcon() { HICON icon=LoadIconW(GetModuleHandleW(nullptr),MAKEINTRESOURCEW(2)); return icon?icon:LoadIconW(nullptr,IDI_APPLICATION); }
struct App {
    Ndi& ndi;
    Engine engine;
    HWND window=nullptr;
    HFONT font=nullptr, titleFont=nullptr;
    HBRUSH background=CreateSolidBrush(RGB(27,29,33)), field=CreateSolidBrush(RGB(40,43,48));
    std::map<int, HWND> controls;
    std::vector<MONITORINFOEXW> monitors;
    std::wstring status=L"就绪", details;
    NOTIFYICONDATAW tray{};
    bool sending=false, stopping=false, closing=false, trayVisible=false, endingSession=false;
    bool applyingRegion=false;
    int centerPreset=3;
    std::wstring preferredMonitor;
    Config config;
    explicit App(Ndi& lib) : ndi(lib), engine(lib) {}
    ~App() { engine.stop(); if (trayVisible) Shell_NotifyIconW(NIM_DELETE,&tray); if (font) DeleteObject(font); if(titleFont) DeleteObject(titleFont); DeleteObject(background); DeleteObject(field); }
    std::wstring configPath() {
        wchar_t local[MAX_PATH]; if (FAILED(SHGetFolderPathW(nullptr,CSIDL_LOCAL_APPDATA,nullptr,0,local))) return L"";
        std::wstring directory=std::wstring(local)+L"\\ScreenCastSendCpp"; CreateDirectoryW(directory.c_str(),nullptr); return directory+L"\\settings.ini";
    }
    void load() {
        config.fps=240; config.cursor=false;
        MONITORINFO info{sizeof(info)}; GetMonitorInfoW(MonitorFromPoint({0,0},MONITOR_DEFAULTTOPRIMARY),&info);
        config.x=info.rcMonitor.left; config.y=info.rcMonitor.top; config.width=(info.rcMonitor.right-info.rcMonitor.left)&~1; config.height=info.rcMonitor.bottom-info.rcMonitor.top;
        if(config.width>=640 && config.height>=640) {
            RECT r=centeredRegion(info.rcMonitor,640); config.x=r.left; config.y=r.top; config.width=config.height=640;
        } else centerPreset=0;
        std::wstring path=configPath(); if (path.empty() || GetFileAttributesW(path.c_str())==INVALID_FILE_ATTRIBUTES) return;
        Config candidate=config;
        auto integer=[&](const wchar_t* key,int fallback) { return int(GetPrivateProfileIntW(L"capture",key,fallback,path.c_str())); };
        candidate.x=integer(L"x",config.x); candidate.y=integer(L"y",config.y); candidate.width=integer(L"width",config.width); candidate.height=integer(L"height",config.height); candidate.fps=integer(L"fps",config.fps);
        candidate.unlimited=integer(L"unlimited",0)!=0; candidate.cursor=integer(L"cursor",config.cursor?1:0)!=0;
        candidate.precision=integer(L"lowLatency",integer(L"precision",0))!=0;
        int backend=integer(L"backend",0); if(backend>=0 && backend<=2) candidate.backend=Backend(backend);
        int pixel=integer(L"pixel",1); if(pixel>=0 && pixel<=1) candidate.pixel=PixelFormat(pixel);
        wchar_t name[256]; GetPrivateProfileStringW(L"capture",L"name",L"Screen Region",name,256,path.c_str()); candidate.name=name;
        wchar_t monitor[64]; GetPrivateProfileStringW(L"capture",L"monitor",L"",monitor,64,path.c_str()); preferredMonitor=monitor;
        try { candidate.validate(); config=candidate; centerPreset=integer(L"centerPreset",0); if(centerPreset<0 || centerPreset>4) centerPreset=0; }
        catch(...) { status=L"已使用当前主屏配置"; }
    }
    void save() {
        std::wstring path=configPath(); if(path.empty()) throw std::runtime_error("Cannot locate settings folder");
        if(GetFileAttributesW(path.c_str())==INVALID_FILE_ATTRIBUTES) {
            HANDLE file=CreateFileW(path.c_str(),GENERIC_WRITE,0,nullptr,CREATE_NEW,FILE_ATTRIBUTE_NORMAL,nullptr);
            if(file==INVALID_HANDLE_VALUE) throw std::runtime_error("Cannot create settings file");
            const unsigned char bom[]{0xff,0xfe}; DWORD written=0;
            bool ok=WriteFile(file,bom,sizeof(bom),&written,nullptr)!=FALSE && written==sizeof(bom); CloseHandle(file);
            if(!ok) throw std::runtime_error("Cannot initialize Unicode settings");
        }
        auto write=[&](const wchar_t* key,const std::wstring& value) { if(!WritePrivateProfileStringW(L"capture",key,value.c_str(),path.c_str())) throw std::runtime_error("Cannot save settings"); };
        write(L"name",config.name); write(L"x",std::to_wstring(config.x)); write(L"y",std::to_wstring(config.y));
        write(L"width",std::to_wstring(config.width)); write(L"height",std::to_wstring(config.height)); write(L"fps",std::to_wstring(config.fps));
        write(L"unlimited",config.unlimited?L"1":L"0"); write(L"cursor",config.cursor?L"1":L"0"); write(L"backend",std::to_wstring(int(config.backend)));
        write(L"lowLatency",config.precision?L"1":L"0"); write(L"precision",config.precision?L"1":L"0");
        write(L"pixel",std::to_wstring(int(config.pixel)));
        write(L"centerPreset",std::to_wstring(centerPreset));
        int selected=int(SendMessageW(controls[MonitorId],CB_GETCURSEL,0,0));
        if(selected>=0 && size_t(selected)<monitors.size()) write(L"monitor",monitors[selected].szDevice);
    }
    HWND add(int id,const wchar_t* kind,const wchar_t* text,DWORD style) {
        HWND c=CreateWindowExW(wcscmp(kind,L"EDIT")==0?WS_EX_CLIENTEDGE:0,kind,text,WS_CHILD|WS_VISIBLE|style,0,0,1,1,window,HMENU(INT_PTR(id)),GetModuleHandleW(nullptr),nullptr);
        if(!c) throw std::runtime_error("Cannot create UI control");
        SendMessageW(c,WM_SETFONT,WPARAM(font),TRUE); controls[id]=c; return c;
    }
    int number(int id) {
        wchar_t text[64]; GetWindowTextW(controls[id],text,64); wchar_t* end=nullptr; long n=wcstol(text,&end,10);
        if(!text[0] || *end || n < -100000 || n > 100000) throw std::runtime_error("Enter valid integer coordinates, size and FPS"); return int(n);
    }
    bool checked(int id) const { return SendMessageW(controls.at(id),BM_GETCHECK,0,0)==BST_CHECKED; }
    void set(int id,int value) { SetWindowTextW(controls[id],std::to_wstring(value).c_str()); }
    void customRegion() {
        centerPreset=0;
        if(controls.count(CenterId)) SendMessageW(controls[CenterId],CB_SETCURSEL,0,0);
    }
    void applyRegion(RECT r,bool centered=false) {
        applyingRegion=true;
        set(XId,r.left); set(YId,r.top); set(WidthId,(r.right-r.left)&~1); set(HeightId,config.pixel==PixelFormat::Nv12?(r.bottom-r.top)&~1:r.bottom-r.top);
        applyingRegion=false;
        if(!centered && controls.count(CenterId)) customRegion();
    }
    void applyCenter() {
        if(centerPreset==0 || sending || stopping) return;
        int i=int(SendMessageW(controls[MonitorId],CB_GETCURSEL,0,0));
        if(i<0 || size_t(i)>=monitors.size()) { customRegion(); return; }
        constexpr int sizes[]{0,320,416,640,512};
        try { applyRegion(centeredRegion(monitors[i].rcMonitor,sizes[centerPreset]),true); }
        catch(const std::exception&) { customRegion(); status=L"中心区域超出所选显示器"; InvalidateRect(window,nullptr,FALSE); }
    }
    void read() {
        wchar_t name[256]; GetWindowTextW(controls[NameId],name,256); config.name=name;
        config.x=number(XId); config.y=number(YId); config.width=number(WidthId); config.height=number(HeightId); config.fps=number(FpsId);
        config.unlimited=checked(UnlimitedId); config.cursor=checked(CursorId); config.precision=checked(PrecisionId);
        config.backend=Backend(SendMessageW(controls[BackendId],CB_GETCURSEL,0,0));
        config.pixel=PixelFormat(SendMessageW(controls[FormatId],CB_GETCURSEL,0,0)); config.validate();
    }
    void setSending(bool value) {
        sending=value;
        for(int id: {NameId,XId,YId,WidthId,HeightId,FpsId,UnlimitedId,BackendId,CursorId,MonitorId,FullId,PickId,StartId,FormatId,CenterId,PrecisionId}) EnableWindow(controls[id],!value && !stopping);
        if(!value) EnableWindow(controls[FpsId],!checked(UnlimitedId) && !stopping);
        EnableWindow(controls[StopId],value && !stopping);
    }
    void updatePreview() { engine.previewEnabled=checked(PreviewId) && IsWindowVisible(window) && !IsIconic(window); }
    void begin() {
        if(sending || stopping || closing) return;
        try { read(); updatePreview(); engine.start(config); setSending(true); status=L"发送中"; try { save(); } catch(const std::exception& e) { details=L"配置保存失败："+wide(e.what()); } }
        catch(const std::exception& e) { MessageBoxW(window,wide(e.what()).c_str(),L"ScreenCast Send C++",MB_OK|MB_ICONWARNING); }
    }
    void stop() {
        if(!sending || stopping) return;
        stopping=true; engine.requestStop(); setSending(true); status=L"正在停止";
    }
    void restore() { ShowWindow(window,SW_RESTORE); SetForegroundWindow(window); if(trayVisible) { Shell_NotifyIconW(NIM_DELETE,&tray); trayVisible=false; } updatePreview(); }
    void hide() {
        if(!trayVisible) trayVisible=Shell_NotifyIconW(NIM_ADD,&tray)!=FALSE;
        if(trayVisible) { ShowWindow(window,SW_HIDE); engine.previewEnabled=false; }
        else { ShowWindow(window,SW_RESTORE); updatePreview(); status=L"无法添加托盘图标，窗口保持打开"; InvalidateRect(window,nullptr,FALSE); }
    }
    void exit() {
        if(closing) return;
        closing=true;
        if(sending) stop(); else DestroyWindow(window);
    }
    void tick() {
        if(endingSession) return;
        Status current=engine.status();
        if(sending && !engine.running) {
            engine.stop(); stopping=false; setSending(false); status=current.error.empty()?L"已停止":L"错误";
            if(!current.error.empty() && !closing) { restore(); MessageBoxW(window,current.error.c_str(),L"发送已停止",MB_OK|MB_ICONWARNING); }
            if(closing) { DestroyWindow(window); return; }
        }
        if(sending && !stopping) {
            wchar_t text[256]; swprintf(text,256,L"%.1f fps · %d 接收端 · 采集 %.2f / 转换 %.2f / 提交 %.2f ms",engine.actualFps.load(),engine.receivers.load(),current.captureMs,current.conversionMs,current.submitMs);
            status=text; details=current.source+L"  ·  "+current.backend;
        }
        InvalidateRect(window,nullptr,FALSE);
    }
    void create() {
        font=CreateFontW(-16,0,0,0,FW_NORMAL,FALSE,FALSE,FALSE,DEFAULT_CHARSET,OUT_DEFAULT_PRECIS,CLIP_DEFAULT_PRECIS,CLEARTYPE_QUALITY,DEFAULT_PITCH,L"Microsoft YaHei UI");
        titleFont=CreateFontW(-25,0,0,0,FW_BOLD,FALSE,FALSE,FALSE,DEFAULT_CHARSET,OUT_DEFAULT_PRECIS,CLIP_DEFAULT_PRECIS,CLEARTYPE_QUALITY,DEFAULT_PITCH,L"Microsoft YaHei UI");
        load(); add(NameId,L"EDIT",config.name.c_str(),WS_TABSTOP|ES_AUTOHSCROLL); SendMessageW(controls[NameId],EM_SETLIMITTEXT,180,0);
        for(int id:{XId,YId,WidthId,HeightId,FpsId}) add(id,L"EDIT",L"",WS_TABSTOP|ES_AUTOHSCROLL);
        applyRegion(config.region()); set(FpsId,config.fps);
        add(UnlimitedId,L"BUTTON",L"无上限",WS_TABSTOP|BS_AUTOCHECKBOX); add(CursorId,L"BUTTON",L"显示鼠标",WS_TABSTOP|BS_AUTOCHECKBOX); add(PreviewId,L"BUTTON",L"预览",WS_TABSTOP|BS_AUTOCHECKBOX); add(PrecisionId,L"BUTTON",L"精确调度",WS_TABSTOP|BS_AUTOCHECKBOX);
        SendMessageW(controls[UnlimitedId],BM_SETCHECK,config.unlimited?BST_CHECKED:BST_UNCHECKED,0); SendMessageW(controls[CursorId],BM_SETCHECK,config.cursor?BST_CHECKED:BST_UNCHECKED,0); SendMessageW(controls[PreviewId],BM_SETCHECK,BST_UNCHECKED,0);
        SendMessageW(controls[PrecisionId],BM_SETCHECK,config.precision?BST_CHECKED:BST_UNCHECKED,0);
        add(BackendId,L"COMBOBOX",L"",WS_TABSTOP|CBS_DROPDOWNLIST|WS_VSCROLL);
        for(auto text:{L"自动 · 优先 DXGI",L"DXGI · GPU 采集",L"GDI · 兼容采集"}) SendMessageW(controls[BackendId],CB_ADDSTRING,0,LPARAM(text));
        SendMessageW(controls[BackendId],CB_SETCURSEL,int(config.backend),0);
        add(FormatId,L"COMBOBOX",L"",WS_TABSTOP|CBS_DROPDOWNLIST|WS_VSCROLL);
        for(auto text:{L"BGRX · RGB 输入",L"NV12 · YUV 4:2:0"}) SendMessageW(controls[FormatId],CB_ADDSTRING,0,LPARAM(text));
        SendMessageW(controls[FormatId],CB_SETCURSEL,int(config.pixel),0);
        add(CenterId,L"COMBOBOX",L"",WS_TABSTOP|CBS_DROPDOWNLIST|WS_VSCROLL);
        for(auto text:{L"自定义区域",L"屏幕中心 320 × 320",L"屏幕中心 416 × 416",L"屏幕中心 640 × 640",L"屏幕中心 512 × 512"}) SendMessageW(controls[CenterId],CB_ADDSTRING,0,LPARAM(text));
        SendMessageW(controls[CenterId],CB_SETCURSEL,centerPreset,0);
        add(MonitorId,L"COMBOBOX",L"",WS_TABSTOP|CBS_DROPDOWNLIST|WS_VSCROLL); refreshMonitors();
        add(FullId,L"BUTTON",L"整个显示器",WS_TABSTOP|BS_PUSHBUTTON); add(PickId,L"BUTTON",L"框选区域",WS_TABSTOP|BS_PUSHBUTTON);
        add(StartId,L"BUTTON",L"开始发送",WS_TABSTOP|BS_PUSHBUTTON); add(StopId,L"BUTTON",L"停止",WS_TABSTOP|BS_PUSHBUTTON); add(TrayId,L"BUTTON",L"收起到托盘",WS_TABSTOP|BS_PUSHBUTTON);
        add(LinkId,L"BUTTON",L"NDI® 网站",WS_TABSTOP|BS_PUSHBUTTON); add(AboutId,L"BUTTON",L"关于",WS_TABSTOP|BS_PUSHBUTTON);
        tray.cbSize=sizeof(tray); tray.hWnd=window; tray.uID=1; tray.uFlags=NIF_MESSAGE|NIF_ICON|NIF_TIP; tray.uCallbackMessage=TrayMessage;
        tray.hIcon=appIcon(); wcscpy_s(tray.szTip,L"ScreenCast Send C++");
        setSending(false); SetTimer(window,1,250,nullptr); layout();
    }
    void refreshMonitors() {
        std::wstring selected=preferredMonitor;
        int previous=int(SendMessageW(controls[MonitorId],CB_GETCURSEL,0,0));
        if(previous>=0 && size_t(previous)<monitors.size()) selected=monitors[previous].szDevice;
        monitors.clear(); SendMessageW(controls[MonitorId],CB_RESETCONTENT,0,0);
        EnumDisplayMonitors(nullptr,nullptr,[](HMONITOR monitor,HDC,LPRECT,LPARAM data)->BOOL {
            App* app=reinterpret_cast<App*>(data); MONITORINFOEXW info{}; info.cbSize=sizeof(info);
            if(GetMonitorInfoW(monitor,reinterpret_cast<MONITORINFO*>(&info))) app->monitors.push_back(info); return TRUE;
        },LPARAM(this));
        for(auto& m:monitors) { std::wstring text=std::wstring(m.szDevice)+L"  "+std::to_wstring(m.rcMonitor.right-m.rcMonitor.left)+L" × "+std::to_wstring(m.rcMonitor.bottom-m.rcMonitor.top); SendMessageW(controls[MonitorId],CB_ADDSTRING,0,LPARAM(text.c_str())); }
        int choice=0;
        for(size_t i=0;i<monitors.size();++i)
            if((selected.empty() && (monitors[i].dwFlags&MONITORINFOF_PRIMARY)) || selected==monitors[i].szDevice) choice=int(i);
        if(!monitors.empty()) SendMessageW(controls[MonitorId],CB_SETCURSEL,choice,0);
        applyCenter();
    }
    void layout() {
        RECT r; GetClientRect(window,&r); int w=r.right, bottom=r.bottom; int y=bottom-345;
        auto move=[&](int id,int x,int top,int width,int h=30){ MoveWindow(controls[id],x,top,width,h,TRUE); };
        move(LinkId,w-154,24,130,30); move(NameId,24,y+23,w-48); move(MonitorId,24,y+67,w-196,200); move(FullId,w-160,y+67,136,32);
        int step=(w-48)/4; for(int i=0;i<4;++i) move(XId+i,24+i*step,y+135,step-12);
        move(FpsId,63,y+183,70); move(UnlimitedId,144,y+183,86); move(BackendId,242,y+183,190,200); move(CursorId,448,y+183,120);
        move(PrecisionId,580,y+183,100);
        move(PickId,24,y+228,120,34); move(FormatId,160,y+228,210,200); move(CenterId,384,y+228,w-408,200); move(StartId,24,y+276,120,36); move(StopId,156,y+276,88,36); move(TrayId,256,y+276,140,36); move(PreviewId,410,y+276,90,36); move(AboutId,w-104,y+276,80,36);
    }
    void paint(HDC dc) {
        RECT client; GetClientRect(window,&client); FillRect(dc,&client,background); SetBkMode(dc,TRANSPARENT); SetTextColor(dc,RGB(232,237,240)); SelectObject(dc,titleFont);
        RECT title{24,24,client.right-180,60}; DrawTextW(dc,L"ScreenCast Send C++",-1,&title,DT_LEFT|DT_SINGLELINE|DT_VCENTER);
        int y=client.bottom-345; RECT preview{24,80,client.right-24,y-16}; HBRUSH black=CreateSolidBrush(RGB(15,17,20)); FillRect(dc,&preview,black); DeleteObject(black);
        SelectObject(dc,font); SetTextColor(dc,RGB(163,172,183));
        if(sending && checked(PreviewId)) engine.drawPreview(dc,preview); else DrawTextW(dc,checked(PreviewId)?L"待发送":L"预览已关闭",-1,&preview,DT_CENTER|DT_VCENTER|DT_SINGLELINE);
        auto label=[&](const wchar_t* text,int x,int top){ RECT r{x,top,client.right-24,top+24}; DrawTextW(dc,text,-1,&r,DT_LEFT|DT_SINGLELINE); };
        label(L"源名称",24,y); int step=(client.right-48)/4; const wchar_t* labels[]{L"X",L"Y",L"宽度 / px",L"高度 / px"};
        for(int i=0;i<4;++i) label(labels[i],24+i*step,y+109); label(L"帧率",24,y+187);
        RECT statusArea{24,client.bottom-28,client.right-24,client.bottom-5}; DrawTextW(dc,status.c_str(),-1,&statusArea,DT_LEFT|DT_SINGLELINE|DT_END_ELLIPSIS);
        RECT sourceArea{24,64,client.right-24,81}; DrawTextW(dc,details.c_str(),-1,&sourceArea,DT_LEFT|DT_SINGLELINE|DT_END_ELLIPSIS);
    }
};

struct Picker {
    RECT bounds{}, selected{};
    POINT anchor{}, current{};
    std::unique_ptr<Buffer> image;
    HBITMAP bitmap=nullptr;
    bool dragging=false, done=false, accepted=false;
    ~Picker(){ if(bitmap) DeleteObject(bitmap); }
    RECT selection() const {
        int w=abs(current.x-anchor.x)&~1;
        return {(std::min)(anchor.x,current.x),(std::min)(anchor.y,current.y),(std::min)(anchor.x,current.x)+w,(std::max)(anchor.y,current.y)};
    }
};
LRESULT CALLBACK pickerProc(HWND window,UINT msg,WPARAM wp,LPARAM lp) {
    Picker* p=reinterpret_cast<Picker*>(GetWindowLongPtrW(window,GWLP_USERDATA));
    if(msg==WM_NCCREATE) { p=static_cast<Picker*>(reinterpret_cast<CREATESTRUCTW*>(lp)->lpCreateParams); SetWindowLongPtrW(window,GWLP_USERDATA,LONG_PTR(p)); }
    if(!p) return DefWindowProcW(window,msg,wp,lp);
    switch(msg) {
    case WM_LBUTTONDOWN: p->anchor=p->current={GET_X_LPARAM(lp),GET_Y_LPARAM(lp)}; p->dragging=true; SetCapture(window); return 0;
    case WM_MOUSEMOVE: if(p->dragging) { p->current={std::clamp<int>(GET_X_LPARAM(lp),0,p->bounds.right-p->bounds.left),std::clamp<int>(GET_Y_LPARAM(lp),0,p->bounds.bottom-p->bounds.top)}; InvalidateRect(window,nullptr,FALSE); } return 0;
    case WM_LBUTTONUP:
        if(p->dragging) { p->dragging=false; ReleaseCapture(); RECT r=p->selection(); if(r.right-r.left>=2 && r.bottom-r.top>=2) { OffsetRect(&r,p->bounds.left,p->bounds.top); p->selected=r; p->accepted=p->done=true; DestroyWindow(window); } } return 0;
    case WM_RBUTTONDOWN: case WM_CLOSE: p->done=true; DestroyWindow(window); return 0;
    case WM_DESTROY: p->done=true; return 0;
    case WM_KEYDOWN: if(wp==VK_ESCAPE) { p->done=true; DestroyWindow(window); } return 0;
    case WM_PAINT: {
        PAINTSTRUCT ps; HDC dc=BeginPaint(window,&ps); HDC source=CreateCompatibleDC(dc); HGDIOBJ old=SelectObject(source,p->bitmap);
        int width=p->bounds.right-p->bounds.left,height=p->bounds.bottom-p->bounds.top; BitBlt(dc,0,0,width,height,source,0,0,SRCCOPY);
        RECT r=p->selection(); HPEN pen=CreatePen(PS_SOLID,2,RGB(35,224,155)); HGDIOBJ oldPen=SelectObject(dc,pen), oldBrush=SelectObject(dc,GetStockObject(HOLLOW_BRUSH));
        Rectangle(dc,r.left,r.top,r.right,r.bottom); SelectObject(dc,oldBrush); SelectObject(dc,oldPen); DeleteObject(pen);
        if(p->dragging) { wchar_t text[80]; swprintf(text,80,L"%ld × %ld",r.right-r.left,r.bottom-r.top); SetTextColor(dc,RGB(255,255,255)); SetBkColor(dc,RGB(0,0,0)); TextOutW(dc,r.left,(std::max)(0L,r.top-22),text,int(wcslen(text))); }
        SelectObject(source,old); DeleteDC(source); EndPaint(window,&ps); return 0;
    }
    case WM_ERASEBKGND:return 1;
    } return DefWindowProcW(window,msg,wp,lp);
}
void pickRegion(App& app) {
    if(app.sending || app.stopping) return;
    ShowWindow(app.window,SW_HIDE); Sleep(150);
    try {
        Picker p; p.bounds=desktopBounds(); Config cfg; cfg.x=p.bounds.left; cfg.y=p.bounds.top; cfg.width=p.bounds.right-p.bounds.left; cfg.height=p.bounds.bottom-p.bounds.top; cfg.cursor=false;
        p.image=std::make_unique<Buffer>(cfg.width,cfg.height); p.image->capture(cfg);
        BITMAPINFO info{}; info.bmiHeader.biSize=sizeof(BITMAPINFOHEADER); info.bmiHeader.biWidth=cfg.width; info.bmiHeader.biHeight=-cfg.height; info.bmiHeader.biPlanes=1; info.bmiHeader.biBitCount=32;
        HDC screen=GetDC(nullptr); void* pixels; p.bitmap=CreateDIBSection(screen,&info,DIB_RGB_COLORS,&pixels,nullptr,0); ReleaseDC(nullptr,screen);
        if(!p.bitmap) throw std::runtime_error("Cannot allocate selector snapshot"); memcpy(pixels,p.image->data,size_t(cfg.width)*cfg.height*4);
        HWND picker=CreateWindowExW(WS_EX_TOPMOST|WS_EX_TOOLWINDOW,L"CastRegionPicker",L"",WS_POPUP,p.bounds.left,p.bounds.top,cfg.width,cfg.height,app.window,nullptr,GetModuleHandleW(nullptr),&p);
        if(!picker) throw std::runtime_error("Cannot create region selector"); ShowWindow(picker,SW_SHOW); SetForegroundWindow(picker); SetFocus(picker);
        MSG msg{}; int result=1;
        while(!p.done && (result=GetMessageW(&msg,nullptr,0,0))>0) { TranslateMessage(&msg); DispatchMessageW(&msg); }
        if(result==0) PostQuitMessage(int(msg.wParam));
        if(IsWindow(picker)) DestroyWindow(picker); if(p.accepted) app.applyRegion(p.selected);
    } catch(const std::exception& e) { MessageBoxW(app.window,wide(e.what()).c_str(),L"框选失败",MB_OK|MB_ICONWARNING); }
    if(IsWindow(app.window)) app.restore();
}
LRESULT CALLBACK windowProc(HWND window,UINT msg,WPARAM wp,LPARAM lp) {
    App* app=reinterpret_cast<App*>(GetWindowLongPtrW(window,GWLP_USERDATA));
    if(msg==WM_NCCREATE) { app=static_cast<App*>(reinterpret_cast<CREATESTRUCTW*>(lp)->lpCreateParams); app->window=window; SetWindowLongPtrW(window,GWLP_USERDATA,LONG_PTR(app)); }
    if(!app) return DefWindowProcW(window,msg,wp,lp);
    switch(msg) {
    case WM_CREATE: try { app->create(); } catch(const std::exception& e) { MessageBoxW(window,wide(e.what()).c_str(),L"启动失败",MB_OK|MB_ICONERROR); return -1; } return 0;
    case WM_SIZE: if(app->controls.count(PreviewId)) { app->layout(); app->updatePreview(); } return 0;
    case WM_GETMINMAXINFO: reinterpret_cast<MINMAXINFO*>(lp)->ptMinTrackSize={720,760}; return 0;
    case WM_DISPLAYCHANGE: if(app->controls.count(MonitorId)) app->refreshMonitors(); return 0;
    case WM_TIMER: app->tick(); return 0;
    case WM_ERASEBKGND:return 1;
    case WM_PAINT: { PAINTSTRUCT ps; HDC dc=BeginPaint(window,&ps); app->paint(dc); EndPaint(window,&ps); return 0; }
    case WM_CTLCOLORSTATIC: case WM_CTLCOLORBTN: SetBkColor(HDC(wp),RGB(27,29,33)); SetTextColor(HDC(wp),RGB(230,234,238)); return LRESULT(app->background);
    case WM_CTLCOLOREDIT: SetBkColor(HDC(wp),RGB(40,43,48)); SetTextColor(HDC(wp),RGB(230,234,238)); return LRESULT(app->field);
    case WM_COMMAND:
        if(LOWORD(wp)>=XId && LOWORD(wp)<=HeightId && HIWORD(wp)==EN_CHANGE && !app->applyingRegion && app->controls.count(CenterId)) app->customRegion();
        switch(LOWORD(wp)) {
        case StartId: app->begin(); break;
        case StopId: app->stop(); break;
        case UnlimitedId: if(!app->sending && !app->stopping) EnableWindow(app->controls[FpsId],!app->checked(UnlimitedId)); break;
        case PreviewId: app->updatePreview(); InvalidateRect(window,nullptr,FALSE); break;
        case FullId: { int i=int(SendMessageW(app->controls[MonitorId],CB_GETCURSEL,0,0)); if(i>=0 && size_t(i)<app->monitors.size()) app->applyRegion(app->monitors[i].rcMonitor); break; }
        case PickId: pickRegion(*app); break;
        case CenterId: if(HIWORD(wp)==CBN_SELCHANGE) { app->centerPreset=int(SendMessageW(app->controls[CenterId],CB_GETCURSEL,0,0)); if(app->centerPreset<0) app->customRegion(); app->applyCenter(); } break;
        case MonitorId: if(HIWORD(wp)==CBN_SELCHANGE) app->applyCenter(); break;
        case FormatId: if(HIWORD(wp)==CBN_SELCHANGE) app->config.pixel=PixelFormat(SendMessageW(app->controls[FormatId],CB_GETCURSEL,0,0)); break;
        case TrayId: app->hide(); break;
        case ExitId: app->exit(); break;
        case LinkId: ShellExecuteW(window,L"open",L"https://ndi.video/",nullptr,nullptr,SW_SHOWNORMAL); break;
        case AboutId: MessageBoxW(window,(L"ScreenCast Send C++ 2.4.4-dev\n对齐 2.4.3 · 性能改进\nC++17 · DXGI / D3D11 · Full NDI\n\nNDI® is a registered trademark of Vizrt NDI AB.\nhttps://ndi.video/\n\n"+wide(app->ndi.version())).c_str(),L"关于",MB_OK); break;
        } return 0;
    case TrayMessage:
        if(lp==WM_LBUTTONDBLCLK) app->restore();
        if(lp==WM_RBUTTONUP) { HMENU menu=CreatePopupMenu(); AppendMenuW(menu,MF_STRING,1,L"打开"); AppendMenuW(menu,MF_STRING|(app->sending && !app->stopping?0:MF_GRAYED),2,L"停止发送"); AppendMenuW(menu,MF_STRING,ExitId,L"退出"); POINT p; GetCursorPos(&p); SetForegroundWindow(window); int choice=TrackPopupMenu(menu,TPM_RETURNCMD|TPM_RIGHTBUTTON,p.x,p.y,0,window,nullptr); DestroyMenu(menu); if(choice==1) app->restore(); if(choice==2) app->stop(); if(choice==ExitId) app->exit(); } return 0;
    case WM_CLOSE: if(!app->closing) app->hide(); return 0;
    case WM_QUERYENDSESSION: return TRUE;
    case WM_ENDSESSION:
        if(wp) {
            // Windows terminates the process after session-end handlers return.
            // Signal capture cleanup without waiting for an NDI or driver call here.
            app->endingSession=true;
            app->engine.previewEnabled=false; app->engine.requestStop();
            if(app->trayVisible) { Shell_NotifyIconW(NIM_DELETE,&app->tray); app->trayVisible=false; }
        }
        return 0;
    case WM_DESTROY: KillTimer(window,1); PostQuitMessage(0); return 0;
    } return DefWindowProcW(window,msg,wp,lp);
}

int selfTest(Ndi& ndi,const std::vector<std::wstring>& args);
int WINAPI wWinMain(HINSTANCE instance,HINSTANCE,LPWSTR,int show) {
    BOOL (WINAPI* dpi)(HANDLE)=nullptr;
    FARPROC dpiAddress=GetProcAddress(GetModuleHandleW(L"user32.dll"),"SetProcessDpiAwarenessContext");
    memcpy(&dpi,&dpiAddress,sizeof(dpi));
    if(dpi) dpi(HANDLE(-4)); else SetProcessDPIAware();
    INITCOMMONCONTROLSEX cc{sizeof(cc),ICC_STANDARD_CLASSES}; InitCommonControlsEx(&cc);
    try {
        Ndi ndi; int count=0; LPWSTR* argv=CommandLineToArgvW(GetCommandLineW(),&count); std::vector<std::wstring> args;
        if(argv) { for(int i=1;i<count;++i) args.push_back(argv[i]); LocalFree(argv); }
        if(!args.empty() && (args[0]==L"--self-test" || args[0]==L"--benchmark")) return selfTest(ndi,args);
        WNDCLASSW cls{}; cls.hInstance=instance; cls.lpfnWndProc=windowProc; cls.lpszClassName=L"ScreenCastSendCpp"; cls.hCursor=LoadCursorW(nullptr,IDC_ARROW); cls.hIcon=appIcon(); RegisterClassW(&cls);
        cls.lpfnWndProc=pickerProc; cls.lpszClassName=L"CastRegionPicker"; cls.hCursor=LoadCursorW(nullptr,IDC_CROSS); RegisterClassW(&cls);
        App app(ndi); RECT r{0,0,800,780}; AdjustWindowRectEx(&r,WS_OVERLAPPEDWINDOW|WS_CLIPCHILDREN,FALSE,0);
        HWND window=CreateWindowExW(0,L"ScreenCastSendCpp",L"ScreenCast Send C++",WS_OVERLAPPEDWINDOW|WS_CLIPCHILDREN,CW_USEDEFAULT,CW_USEDEFAULT,r.right-r.left,r.bottom-r.top,nullptr,nullptr,instance,&app);
        if(!window) return 1; ShowWindow(window,show); UpdateWindow(window);
        MSG msg; while(GetMessageW(&msg,nullptr,0,0)>0) { if(!IsDialogMessageW(window,&msg)) { TranslateMessage(&msg); DispatchMessageW(&msg); } }
        return int(msg.wParam);
    } catch(const std::exception& e) { MessageBoxW(nullptr,wide(e.what()).c_str(),L"ScreenCast Send C++",MB_OK|MB_ICONERROR); return 1; }
}
