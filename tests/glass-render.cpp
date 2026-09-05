// Runs the production fragment shader in a real Mesa GLES context. No DRM needed.
#include <EGL/egl.h>
#include <GLES2/gl2.h>
#include <cassert>
#include <cmath>
#include <cstdio>
#include <string>
#include <vector>
#include "../payload/wayfire/glass/glass-shader.hpp"
constexpr int N = 128;
static GLuint shader(GLenum kind, const std::string& source) {
    GLuint s = glCreateShader(kind); const char* p = source.c_str();
    glShaderSource(s, 1, &p, nullptr); glCompileShader(s);
    GLint ok; glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) { char log[4096]; glGetShaderInfoLog(s, sizeof(log), nullptr, log); fprintf(stderr, "%s\n", log); }
    assert(ok); return s;
}
static void replace(std::string& s, const char* token, const char* value) {
    s.replace(s.find(token), std::string(token).size(), value);
}
int main() {
    EGLDisplay d = eglGetDisplay(EGL_DEFAULT_DISPLAY); assert(eglInitialize(d, nullptr, nullptr));
    const EGLint cfg[] = {EGL_SURFACE_TYPE,EGL_PBUFFER_BIT,EGL_RENDERABLE_TYPE,EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE,8,EGL_GREEN_SIZE,8,EGL_BLUE_SIZE,8,EGL_ALPHA_SIZE,8,EGL_NONE};
    EGLConfig c; EGLint n; assert(eglChooseConfig(d,cfg,&c,1,&n) && n);
    const EGLint size[] = {EGL_WIDTH,N,EGL_HEIGHT,N,EGL_NONE};
    EGLSurface surf = eglCreatePbufferSurface(d,c,size);
    const EGLint ctx[] = {EGL_CONTEXT_CLIENT_VERSION,2,EGL_NONE};
    EGLContext context = eglCreateContext(d,c,EGL_NO_CONTEXT,ctx);
    assert(eglMakeCurrent(d,surf,surf,context));
    printf("Renderer: %s\n", glGetString(GL_RENDERER));
    std::string fragment = nethos_glass_fragment_shader;
    replace(fragment,"@builtin_ext@","");
    replace(fragment,"@builtin@","uniform sampler2D client_texture; vec4 get_pixel(vec2 p) {return texture2D(client_texture,p);}");
    const char* vertex = "attribute vec2 pos; varying mediump vec2 uvpos[2]; void main(){gl_Position=vec4(pos,0.,1.);uvpos[0]=(pos+1.)*.5;uvpos[1]=uvpos[0];}";
    GLuint prog=glCreateProgram(); glAttachShader(prog,shader(GL_VERTEX_SHADER,vertex));
    glAttachShader(prog,shader(GL_FRAGMENT_SHADER,fragment)); glLinkProgram(prog);
    GLint ok; glGetProgramiv(prog,GL_LINK_STATUS,&ok); assert(ok); glUseProgram(prog);
    std::vector<unsigned char> client(N*N*4), bg(N*N*4);
    for(int y=0;y<N;y++) for(int x=0;x<N;x++) {
        int i=(y*N+x)*4;
        bool card=x>=16 && x<112 && y>=16 && y<112;
        bool opaque=x>=48 && x<80 && y>=48 && y<80;
        client[i]=opaque?241:card?22:0; client[i+1]=opaque?244:card?27:0;
        client[i+2]=opaque?248:card?34:0; client[i+3]=opaque?255:card?184:0;
        bg[i]=(x/4)%2?220:30; bg[i+1]=y*2; bg[i+2]=120; bg[i+3]=255;
    }
    GLuint tex[2]; glGenTextures(2,tex);
    for(int t=0;t<2;t++) {
        glActiveTexture(GL_TEXTURE0+t); glBindTexture(GL_TEXTURE_2D,tex[t]);
        glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_WRAP_S,GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_WRAP_T,GL_CLAMP_TO_EDGE);
        glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA,N,N,0,GL_RGBA,GL_UNSIGNED_BYTE,t?bg.data():client.data());
    }
    auto uniform=[&](const char* name){return glGetUniformLocation(prog,name);};
    glUniform1i(uniform("client_texture"),0); glUniform1i(uniform("bg_texture"),1);
    glUniform2f(uniform("view_size"),N,N); glUniform2f(uniform("background_texel"),1.f/N,1.f/N);
    glUniform1f(uniform("sat"),1); glUniform1f(uniform("rim_strength"),0);
    const float identity[]={1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1};
    glUniformMatrix4fv(uniform("background_uv_matrix"),1,GL_FALSE,identity);
    float vertices[]={-1,-1,1,-1,-1,1,1,1};
    GLuint attr=glGetAttribLocation(prog,"pos"); glEnableVertexAttribArray(attr);
    glVertexAttribPointer(attr,2,GL_FLOAT,GL_FALSE,0,vertices); glViewport(0,0,N,N);
    std::vector<unsigned char> plain(N*N*4), glass(N*N*4);
    auto draw=[&](float refraction, std::vector<unsigned char>& result){
        glUniform1f(uniform("refraction"),refraction); glDrawArrays(GL_TRIANGLE_STRIP,0,4);
        glReadPixels(0,0,N,N,GL_RGBA,GL_UNSIGNED_BYTE,result.data()); assert(glGetError()==GL_NO_ERROR);
    };
    draw(0,plain); draw(4,glass);
    int changed=0;
    for(int y=0;y<N;y++) for(int x=0;x<N;x++) {
        int i=(y*N+x)*4;
        bool edge=x<22 || x>=106 || y<22 || y>=106;
        for(int k=0;k<4;k++) {
            if(client[i+3]==0 || client[i+3]==255) assert(glass[i+k]==client[i+k]);
            if(!edge) assert(glass[i+k]==plain[i+k]);
            if(glass[i+k]!=plain[i+k]) changed++;
        }
    }
    assert(changed>500);
    // A zero-size transient view is clamped by the compositor; exercise its minimum.
    glUniform2f(uniform("view_size"),1,1); draw(8,glass);
    printf("PASS: %d changed edge channels; opaque text, transparent exterior and centre unchanged; tiny surface valid\n",changed);
    eglMakeCurrent(d,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);
    eglDestroyContext(d,context); eglDestroySurface(d,surf); eglTerminate(d);
}
