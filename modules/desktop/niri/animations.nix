# Short and critically damped: nothing bounces.
{ ... }:

''
  animations {
      window-open {
          duration-ms 150
          curve "ease-out-expo"
      }
      window-close {
          duration-ms 150
          curve "ease-out-quad"
      }
      horizontal-view-movement { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
      window-movement { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
      window-resize { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
      workspace-switch { spring damping-ratio=1.0 stiffness=1000 epsilon=0.0001; }
      config-notification-open-close { spring damping-ratio=1.0 stiffness=1000 epsilon=0.001; }
      overview-open-close { spring damping-ratio=1.0 stiffness=800 epsilon=0.0001; }
      // The fork's canvas: pan / fly-to and zoom (design motion tokens, ashd/tokens/tokens.json).
      canvas-pan { spring damping-ratio=1.0 stiffness=500 epsilon=0.0001; }
      canvas-zoom { spring damping-ratio=1.0 stiffness=600 epsilon=0.0001; }
  }
''
