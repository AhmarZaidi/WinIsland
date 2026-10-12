import { useEffect, useRef } from 'react'
import type { DemoMode, Locale } from '../content'

type ProductDemoProps = {
  locale: Locale
  mode?: DemoMode
  variant?: 'hero' | 'stage'
}

const STAGE_IMAGES: Record<DemoMode, (locale: Locale) => { src: string; width: number; height: number }> = {
  media: () => ({ src: '/island/expanded-afterglow.webp', width: 1106, height: 703 }),
  notification: (locale) => ({ src: `/island/notify-${locale}.webp`, width: 1002, height: 258 }),
  widgets: (locale) => ({ src: `/island/widgets-${locale}.webp`, width: 1106, height: 703 }),
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
      poster="/media/hero-loop-poster.jpg"
      muted
      loop
      playsInline
      preload="auto"
      aria-hidden="true"
    >
      <source src="/media/hero-loop.webm" type="video/webm" />
      <source src="/media/hero-loop.mp4" type="video/mp4" />
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
