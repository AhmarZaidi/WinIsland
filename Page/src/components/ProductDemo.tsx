import { useEffect, useRef } from 'react'
import expandedAfterglow from '../assets/island/expanded-afterglow.webp'
import notifyEn from '../assets/island/notify-en.webp'
import notifyZh from '../assets/island/notify-zh.webp'
import widgetsEn from '../assets/island/widgets-en.webp'
import widgetsZh from '../assets/island/widgets-zh.webp'
import heroLoopMp4 from '../assets/media/hero-loop.mp4'
import heroLoopPoster from '../assets/media/hero-loop-poster.jpg'
import heroLoopWebm from '../assets/media/hero-loop.webm'
import type { DemoMode, Locale } from '../content'

type ProductDemoProps = {
  locale: Locale
  mode?: DemoMode
  variant?: 'hero' | 'stage'
}

const STAGE_IMAGES: Record<DemoMode, (locale: Locale) => { src: string; width: number; height: number }> = {
  media: () => ({ src: expandedAfterglow, width: 1106, height: 703 }),
  notification: (locale) => ({ src: locale === 'zh' ? notifyZh : notifyEn, width: 1002, height: 258 }),
  widgets: (locale) => ({ src: locale === 'zh' ? widgetsZh : widgetsEn, width: 1106, height: 703 }),
}

const MODES: DemoMode[] = ['media', 'notification', 'widgets']

function HeroVideo() {
  const videoRef = useRef<HTMLVideoElement>(null)

  useEffect(() => {
    const video = videoRef.current
    if (!video || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return
    video.play().catch(() => undefined)
  }, [])

  return (
    <video
      ref={videoRef}
      className="hero-video"
      width={2160}
      height={900}
      poster={heroLoopPoster}
      muted
      loop
      playsInline
      preload="auto"
      aria-hidden="true"
    >
      <source src={heroLoopWebm} type="video/webm" />
      <source src={heroLoopMp4} type="video/mp4" />
    </video>
  )
}

export function ProductDemo({ locale, mode = 'media', variant = 'stage' }: ProductDemoProps) {
  if (variant === 'hero') {
    return (
      <div className="product-demo product-demo--hero">
        <span className="demo-halo" aria-hidden="true" />
        <div className="hero-screen">
          <HeroVideo />
        </div>
      </div>
    )
  }

  return (
    <div className="product-demo product-demo--stage">
      {MODES.map((item) => {
        const image = STAGE_IMAGES[item](locale)
        return (
          <img
            key={item}
            className={item === mode ? 'stage-island is-active' : 'stage-island'}
            src={image.src}
            width={Math.round(image.width / 2)}
            height={Math.round(image.height / 2)}
            alt=""
            aria-hidden="true"
            decoding="async"
          />
        )
      })}
    </div>
  )
}
