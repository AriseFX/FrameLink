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
#include <vector>
#include <fstream>
#include <map>
#include <sstream>
using namespace cast;

namespace {
struct PatternState {
    int sequence=0, offset=0, width=0, height=0;
    int cellWidth=48, cellHeight=100;
    HDC source=nullptr;
    HBITMAP bitmap=nullptr;
    HGDIOBJ old=nullptr;
    ~PatternState() { if(source && old) SelectObject(source,old); if(bitmap) DeleteObject(bitmap); if(source) DeleteDC(source); }
    void motionImage(const Config& config) {
        Buffer image(config.width,config.height); image.capture(config); width=config.width; height=config.height;
        BITMAPINFO info{}; info.bmiHeader.biSize=sizeof(BITMAPINFOHEADER); info.bmiHeader.biWidth=width; info.bmiHeader.biHeight=-height;
        info.bmiHeader.biPlanes=1; info.bmiHeader.biBitCount=32;
        void* data=nullptr; source=CreateCompatibleDC(nullptr); bitmap=CreateDIBSection(source,&info,DIB_RGB_COLORS,&data,nullptr,0);
        if(!source || !bitmap) throw std::runtime_error("Cannot create moving desktop test image");
        old=SelectObject(source,bitmap); memcpy(data,image.data,size_t(width)*height*4);
    }
};
LRESULT CALLBACK patternProc(HWND window,UINT msg,WPARAM wp,LPARAM lp) {
    auto* state=reinterpret_cast<PatternState*>(GetWindowLongPtrW(window,GWLP_USERDATA));
    if(msg==WM_NCCREATE) { state=static_cast<PatternState*>(reinterpret_cast<CREATESTRUCTW*>(lp)->lpCreateParams); SetWindowLongPtrW(window,GWLP_USERDATA,LONG_PTR(state)); }
    if(msg==WM_PAINT) {
        PAINTSTRUCT ps; HDC dc=BeginPaint(window,&ps);
        if(state && state->source) {
            int x=state->offset%state->width;
            BitBlt(dc,0,0,state->width-x,state->height,state->source,x,0,SRCCOPY);
            BitBlt(dc,state->width-x,0,x,state->height,state->source,0,0,SRCCOPY);
        }
        int cell=state?state->cellWidth:48, height=state?state->cellHeight:100;
        for(int i=0;i<8;++i) { int gray=(state && (state->sequence&(1<<i)))?224:32; HBRUSH brush=CreateSolidBrush(RGB(gray,gray,gray)); RECT r{i*cell,0,(i+1)*cell,height}; FillRect(dc,&r,brush); DeleteObject(brush); }
        EndPaint(window,&ps); return 0;
    }
    return DefWindowProcW(window,msg,wp,lp);
}
void pump(int ms,HWND animated=nullptr,PatternState* state=nullptr) {
    double until=milliseconds()+ms, next=milliseconds(); MSG msg;
    while(milliseconds()<until) {
        while(PeekMessageW(&msg,nullptr,0,0,PM_REMOVE)) { TranslateMessage(&msg); DispatchMessageW(&msg); }
        double now=milliseconds();
        if(animated && state && state->source && now>=next) {
            ++state->offset; InvalidateRect(animated,nullptr,FALSE); UpdateWindow(animated); next=now+1000.0/240;
        }
        Sleep(1);
    }
}
void require(bool value,const char* reason) { if(!value) throw std::runtime_error(reason); }
}
int selfTest(Ndi& ndi,const std::vector<std::wstring>& args) {
    std::ostringstream log; std::ofstream output("native-test.txt",std::ios::trunc);
    HWND pattern=nullptr; void* receiver=nullptr; std::thread reader; std::atomic<bool> done{false};
    std::mutex lock; std::map<int,double> paints,samples,submitSamples; std::string readerError;
    std::atomic<uint64_t> receivedFrames{0};
    try {
        bool benchmark=args[0]==L"--benchmark";
        for(int size:{320,416,512,640}) {
            RECT centered=centeredRegion({-2560,-1440,0,0},size);
            require(centered.left==-2560+(2560-size)/2 && centered.top==-1440+(1440-size)/2 && centered.right-centered.left==size && centered.bottom-centered.top==size,"Negative monitor center incorrect");
        }
        bool rejectedCenter=false; try { centeredRegion({0,0,400,400},640); } catch(...) { rejectedCenter=true; }
        require(rejectedCenter,"Oversized center region accepted");
        Config config; config.name=L"Native verification"; config.x=80; config.y=80; config.width=benchmark?1920:640; config.height=benchmark?1080:360; config.cursor=false;
        config.fps=args.size()>1?std::stoi(args[1]):60; config.unlimited=args.size()>2 && args[2]==L"unlimited";
        config.backend=args.size()>3 && args[3]==L"GDI"?Backend::Gdi:Backend::Dxgi;
        if(std::find(args.begin(),args.end(),L"bgrx")!=args.end()) config.pixel=PixelFormat::Bgrx;
        config.precision=std::find(args.begin(),args.end(),L"precision")!=args.end();
        config.cursor=std::find(args.begin(),args.end(),L"cursor")!=args.end();
        for(const auto& arg:args) if(arg.rfind(L"size=",0)==0) {
            std::wstring value=arg.substr(5); size_t consumed=0; int size=std::stoi(value,&consumed);
            require(consumed==value.size() && size>=64 && size<=4096 && !(size&1),"size= requires an even square size between 64 and 4096");
            config.width=config.height=size;
            MONITORINFO monitor{sizeof(monitor)};
            require(GetMonitorInfoW(MonitorFromPoint({0,0},MONITOR_DEFAULTTOPRIMARY),&monitor)!=FALSE,"Cannot locate primary monitor");
            RECT region=centeredRegion(monitor.rcMonitor,size); config.x=region.left; config.y=region.top;
        }
        Config invalid=config; invalid.width=319; bool rejected=false; try{invalid.validate();}catch(...){rejected=true;} require(rejected,"Odd width accepted");
        invalid=config; invalid.fps=1001; rejected=false; try{invalid.validate();}catch(...){rejected=true;} require(rejected,"FPS >1000 accepted");
        invalid=config; invalid.pixel=PixelFormat::Nv12; invalid.height=359; rejected=false; try{invalid.validate();}catch(...){rejected=true;} require(rejected,"Odd NV12 height accepted");
        Buffer color(2,2); for(int i=0;i<4;++i) { color.data[i*4]=0; color.data[i*4+1]=0; color.data[i*4+2]=255; color.data[i*4+3]=255; }
        Nv12Buffer converted(2,2); auto* red=converted.convert(color);
        require(abs(int(red[0])-82)<=2 && abs(int(red[4])-90)<=2 && abs(int(red[5])-240)<=2,"NV12 channel order or plane layout incorrect");
        // GPU readback rows have padding. The direct path must produce the same
        // bytes as a packed DIB, including the second half of the UV plane.
        for(int size:{512,640}) {
            Buffer packed(size,size); int pitch=size*4+64;
            std::vector<uint8_t> padded(size_t(pitch)*size,0xcd);
            for(int y=0;y<size;++y) {
                for(int x=0;x<size;++x) {
                    auto* pixel=packed.data+y*packed.stride+x*4;
                    pixel[0]=uint8_t(x*3+y); pixel[1]=uint8_t(x+y*5); pixel[2]=uint8_t(x*7+y*11); pixel[3]=255;
                }
                memcpy(padded.data()+size_t(y)*pitch,packed.data+size_t(y)*packed.stride,packed.stride);
            }
            Nv12Buffer packedNv12(size,size), pitchedNv12(size,size);
            auto* expected=packedNv12.convert(packed);
            auto* actual=pitchedNv12.convert(padded.data(),pitch,size,size);
            require(memcmp(expected,actual,size_t(size)*size*3/2)==0,"Pitched direct NV12 differs from packed input");
            bool badPitch=false;
            try { pitchedNv12.convert(padded.data(),size*4-1,size,size); } catch(const std::exception&) { badPitch=true; }
            require(badPitch,"Invalid GPU pitch accepted");
        }
        bool motion=std::find(args.begin(),args.end(),L"motion")!=args.end(); PatternState state;
        state.cellWidth=(std::min)(48,config.width/8); state.cellHeight=(std::min)(100,config.height);
        if(motion) state.motionImage(config);
        WNDCLASSW cls{}; cls.hInstance=GetModuleHandleW(nullptr); cls.lpfnWndProc=patternProc; cls.lpszClassName=L"NativeTestPattern"; RegisterClassW(&cls);
        pattern=CreateWindowExW(WS_EX_TOPMOST|WS_EX_TOOLWINDOW,L"NativeTestPattern",L"",WS_POPUP,config.x,config.y,motion?config.width:state.cellWidth*8,motion?config.height:state.cellHeight,nullptr,nullptr,cls.hInstance,&state);
        require(pattern!=nullptr,"Cannot create pattern"); ShowWindow(pattern,SW_SHOW); UpdateWindow(pattern); pump(300);
        Engine engine(ndi); engine.previewEnabled=std::find(args.begin(),args.end(),L"preview")!=args.end(); engine.start(config);
        bool fastest=args.size()>4 && args[4]==L"fast";
        std::string name=utf8(engine.status().source); RecvSettings settings{{name.c_str(),nullptr},fastest?100:0,100,false,nullptr};
        receiver=ndi.recvCreate(&settings); require(receiver!=nullptr,"Receiver creation failed");
        reader=std::thread([&] {
            try {
                while(!done) {
                    VideoFrame frame{}; int type=ndi.recvCapture(receiver,&frame,nullptr,nullptr,100); if(type!=1) continue;
                    try {
                        require(frame.width==config.width && frame.height==config.height,"Resolution changed in transit"); require(frame.rateD==1 && frame.rateN>0,"Invalid rate metadata");
                        if(!config.unlimited) require(frame.rateN==config.fps,"Wrong locked FPS metadata");
                        int seq=0;
                        bool uyvy=frame.fourcc==0x59565955;
                        require(uyvy || frame.fourcc==BGRX || frame.fourcc==0x41524742,"Unexpected decoded pixel format");
                        for(int i=0;i<8;++i) if(frame.data[(state.cellHeight/2)*frame.stride+(i*state.cellWidth+state.cellWidth/2)*(uyvy?2:4)+(uyvy?1:0)]>128) seq|=1<<i;
                        ++receivedFrames;
                        double received=milliseconds(); std::lock_guard<std::mutex> guard(lock);
                        if(seq>5 && paints.count(seq) && !samples.count(seq)) { samples[seq]=received-paints[seq]; submitSamples[seq]=double(utcTime()-frame.timecode)/10000.0; }
                    } catch(...) { ndi.recvFreeVideo(receiver,&frame); throw; }
                    ndi.recvFreeVideo(receiver,&frame);
                }
            } catch(const std::exception& e) { std::lock_guard<std::mutex> guard(lock); readerError=e.what(); }
        });
        double deadline=milliseconds()+15000;
        while(engine.receivers==0 && milliseconds()<deadline && engine.running) pump(50,pattern,&state);
        require(engine.receivers>0,"Receiver failed to connect"); pump(1000,pattern,&state);
        double receiveBegin=milliseconds(); uint64_t receiveCount=receivedFrames.load(), sendCount=engine.frames.load();
        uint64_t updateCount=engine.imageUpdates.load(), repeatCount=engine.repeatedFrames.load();
        for(int sequence=1;sequence<=55;++sequence) {
            { std::lock_guard<std::mutex> guard(lock); paints[sequence]=milliseconds(); }
            state.sequence=sequence; InvalidateRect(pattern,nullptr,FALSE); UpdateWindow(pattern); pump(150,pattern,&state);
        }
        pump(300); double elapsed=milliseconds()-receiveBegin;
        double receiveFps=(receivedFrames.load()-receiveCount)*1000.0/elapsed;
        double sendFps=(engine.frames.load()-sendCount)*1000.0/elapsed;
        uint64_t updates=engine.imageUpdates.load()-updateCount, repeats=engine.repeatedFrames.load()-repeatCount;
        done=true; reader.join(); require(readerError.empty(),readerError.c_str()); require(samples.size()>=45,"Too few distinct received patterns");
        require(engine.status().error.empty(),"Capture engine failed");
        std::vector<double> latencies,submissions; for(auto& pair:samples) latencies.push_back(pair.second); for(auto& pair:submitSamples) submissions.push_back(pair.second);
        std::sort(latencies.begin(),latencies.end()); std::sort(submissions.begin(),submissions.end());
        log<<"C++17 native 2.4.4-dev, "<<config.width<<"x"<<config.height<<", target="<<(config.unlimited?"unlimited":std::to_string(config.fps))<<", backend="<<utf8(engine.status().backend)<<", receiver="<<(fastest?"fastest":"BGRX/BGRA")<<", motion="<<motion<<", precision="<<config.precision<<", cursor="<<config.cursor<<", preview="<<engine.previewEnabled.load()<<"\n";
        log<<"NDI="<<ndi.version()<<"\n";
        log<<"Patterns="<<samples.size()<<", repaint -> decoded p50="<<latencies[latencies.size()/2]<<" ms, p95="<<latencies[size_t(latencies.size()*.95)]<<" ms\n";
        log<<"Submit -> decoded p50="<<submissions[submissions.size()/2]<<" ms, p95="<<submissions[size_t(submissions.size()*.95)]<<" ms\n";
        log<<"Send format="<<(config.pixel==PixelFormat::Nv12?"NV12":"BGRX")<<", send FPS="<<sendFps<<", decoded FPS="<<receiveFps<<", capture="<<engine.status().captureMs<<" ms, conversion="<<engine.status().conversionMs<<" ms, submit call="<<engine.status().submitMs<<" ms\n";
        log<<"Updated submissions="<<updates<<", repeated submissions="<<repeats<<", updated submissions/sec="<<updates*1000.0/elapsed<<" (DXGI desktop/cursor events; not unique ROI pixels)\n";
        ndi.recvDestroy(receiver); receiver=nullptr; engine.stop();
        engine.start(config); pump(250); double shutdown=milliseconds(); engine.stop(); require(milliseconds()-shutdown<5000,"Stop took >5s");
        log<<"PASS: ABI, settings validation, NV12 color and pitched plane layout (512/640), image patterns, native NDI receive, restart and bounded stop\n";
        DestroyWindow(pattern); pattern=nullptr; output<<log.str(); return 0;
    } catch(const std::exception& e) {
        done=true; if(reader.joinable()) reader.join(); if(receiver) ndi.recvDestroy(receiver); if(pattern) DestroyWindow(pattern);
        output<<log.str()<<"FAIL: "<<e.what()<<"\n"; return 1;
    }
}
